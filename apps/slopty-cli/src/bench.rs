//! `slopty bench echo`: keystroke → first frame back on a `cat` session (transport + host
//! engine + frame coalescing, no terminal emulation on this side).
//!
//! `slopty bench screen`: open a screen stream and report what arrives.
//!
//! Frame latency is capture timestamp → decoded frame available, on the host time clock, so it
//! is only meaningful when client and host share a clock (loopback). Everything else (fps,
//! arrival jitter, loss, FEC, NACKs) holds over any path. `SLOPTY_DROP_PERMILLE` on this
//! process drops incoming media datagrams to simulate a lossy path.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use slopty_client::{HostLink, LinkEvent};
use slopty_core::WindowId;
use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent, ScreenRequest};
use slopty_proto::terminal::{OpenSession, TermEvent, TermRequest, TermSize};
use slopty_proto::{ClientMsg, HostMsg};

use crate::client::{Session, connect_to};

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
/// that comes back. Includes the host's frame coalescing window and one round trip.
pub async fn echo(data_dir: &Path, needle: Option<&str>, count: u32) -> Result<()> {
    let mut session = connect_to(data_dir, needle).await?;
    let size = TermSize { cols: 60, rows: 12, ..TermSize::default() };
    session
        .conn
        .tx
        .send(&ClientMsg::OpenSession(OpenSession {
            size,
            cwd: None,
            command: vec!["/bin/cat".to_owned()],
            env: Vec::new(),
            title: Some("bench echo".to_owned()),
            attach: true,
        }))
        .await?;
    let id = loop {
        match session.conn.rx.recv().await? {
            HostMsg::SessionOpened(s) => break s.id,
            HostMsg::Term { event: TermEvent::Error(e), .. } => bail!("host: {e}"),
            _other => {}
        }
    };
    let Session { conn, endpoint } = session;
    let mut link = HostLink::start(conn);
    let mut events = link.events().context("events")?;

    // Let the attach frames settle before timing anything.
    drain(&mut events, Duration::from_millis(500)).await;

    let mut samples: Vec<u64> = Vec::with_capacity(count as usize);
    let mut timeouts = 0_u32;
    for i in 0..count {
        let byte = if i % 2 == 0 { b"x".to_vec() } else { b"y".to_vec() };
        let sent = Instant::now();
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
    println!("  timeouts {timeouts}  quic rtt {rtt}  paths: {}", link.paths());
    let _closed = link.send(ClientMsg::Term { session: id, req: TermRequest::Close }).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    link.close();
    endpoint.close().await;
    Ok(())
}

/// What to stream and for how long.
#[derive(Debug, Clone, Copy)]
pub struct ScreenBench {
    /// Window id on the host (`slopty bench screen --list` prints them).
    pub window: Option<u32>,
    /// Display id on the host; the main display when neither is given.
    pub display: Option<u32>,
    /// Run length.
    pub seconds: u64,
    /// Capture scale (1.0 = native).
    pub scale: f32,
    /// Frame rate cap.
    pub fps: u16,
    /// Bitrate, Mbit/s.
    pub mbit: u32,
}

/// Print the host's windows and displays.
pub async fn list(data_dir: &Path, needle: Option<&str>) -> Result<()> {
    let session = connect_to(data_dir, needle).await?;
    let Session { conn, endpoint } = session;
    let mut link = HostLink::start(conn);
    let mut events = link.events().context("events")?;
    link.sender().send(ClientMsg::Screen(ScreenRequest::List)).await?;
    loop {
        match events.recv().await {
            Some(LinkEvent::Control(HostMsg::Screen(ScreenEvent::Listing {
                windows,
                displays,
            }))) => {
                for d in displays {
                    println!("display {:<12} {}×{} @{}x {} Hz", d.id, d.w, d.h, d.scale, d.hz);
                }
                for w in windows {
                    println!("window  {:<12} {}×{}  {} — {}", w.id.0, w.w, w.h, w.app, w.title);
                }
                break;
            }
            Some(LinkEvent::Disconnected(why)) => bail!("disconnected: {why}"),
            Some(_other) => {}
            None => bail!("link closed"),
        }
    }
    drop(link);
    endpoint.close().await;
    Ok(())
}

/// Stream for `bench.seconds` and print the numbers.
pub async fn screen(data_dir: &Path, needle: Option<&str>, bench: ScreenBench) -> Result<()> {
    let session = connect_to(data_dir, needle).await?;
    let Session { conn, endpoint } = session;
    let mut link = HostLink::start(conn);
    let mut events = link.events().context("events")?;
    let out = link.sender();

    let target = match (bench.window, bench.display) {
        (Some(w), _) => CaptureTarget::Window(WindowId(w)),
        (None, Some(d)) => CaptureTarget::Display(d),
        (None, None) => {
            out.send(ClientMsg::Screen(ScreenRequest::List)).await?;
            loop {
                match events.recv().await {
                    Some(LinkEvent::Control(HostMsg::Screen(ScreenEvent::Listing {
                        displays,
                        ..
                    }))) => {
                        let main = displays.first().context("host has no display")?;
                        break CaptureTarget::Display(main.id);
                    }
                    Some(LinkEvent::Disconnected(why)) => bail!("disconnected: {why}"),
                    Some(_other) => {}
                    None => bail!("link closed"),
                }
            }
        }
    };
    let quality = Quality {
        fps: bench.fps,
        bitrate_bps: bench.mbit.saturating_mul(1_000_000),
        scale: bench.scale,
        ..Quality::default()
    };
    out.send(ClientMsg::Screen(ScreenRequest::Open { target, quality })).await?;
    let opened_at = Instant::now();
    let (stream, codec, width, height) = loop {
        match events.recv().await {
            Some(LinkEvent::Control(HostMsg::Screen(ScreenEvent::Opened {
                stream,
                codec,
                width,
                height,
                ..
            }))) => break (stream, codec, width, height),
            Some(LinkEvent::Control(HostMsg::Screen(ScreenEvent::Closed { reason, .. }))) => {
                bail!("host closed the stream: {reason}")
            }
            Some(LinkEvent::Disconnected(why)) => bail!("disconnected: {why}"),
            Some(_other) => {}
            None => bail!("link closed"),
        }
    };
    println!(
        "{}: {target:?} → {width}×{height} {codec:?} {} fps {} Mbit/s scale {}",
        link.ack().name,
        bench.fps,
        bench.mbit,
        bench.scale
    );
    let handle = link.screen(stream, codec);
    let mut frames = handle.frames();
    let deadline = tokio::time::Instant::now()
        .checked_add(Duration::from_secs(bench.seconds))
        .context("run length")?;

    let mut latency_us: Vec<u64> = Vec::new();
    let mut gaps_us: Vec<u64> = Vec::new();
    let mut first_frame: Option<Instant> = None;
    let mut last_arrival: Option<Instant> = None;
    loop {
        tokio::select! {
            changed = frames.changed() => {
                if changed.is_err() {
                    break;
                }
                let frame = frames.borrow_and_update().clone();
                let Some(frame) = frame else { continue };
                let now = Instant::now();
                first_frame.get_or_insert(now);
                // The wire carries the low 32 bits of the host clock in microseconds.
                #[expect(clippy::cast_possible_truncation, reason = "low 32 bits by design")]
                let (now_lo, pts_lo) = (slopty_capture::host_now_us() as u32, frame.pts_us as u32);
                // ScreenCaptureKit stamps a frame with its display time, which can sit a
                // fraction of a millisecond ahead of "now"; clamp those to zero.
                let lat = i64::from(now_lo.wrapping_sub(pts_lo).cast_signed()).max(0);
                latency_us.push(u64::try_from(lat).unwrap_or(0));
                if let Some(prev) = last_arrival {
                    let gap = now.saturating_duration_since(prev).as_micros();
                    gaps_us.push(u64::try_from(gap).unwrap_or(u64::MAX));
                }
                last_arrival = Some(now);
            }
            () = tokio::time::sleep_until(deadline) => break,
            ev = events.recv() => {
                match ev {
                    Some(LinkEvent::Control(HostMsg::Screen(ScreenEvent::Closed { reason, .. }))) => {
                        eprintln!("host closed the stream: {reason}");
                        break;
                    }
                    Some(LinkEvent::Disconnected(why)) => {
                        eprintln!("disconnected: {why}");
                        break;
                    }
                    Some(_other) => {}
                    None => break,
                }
            }
        }
    }
    let stats = handle.stats();
    let run = first_frame.map_or(Duration::ZERO, |t| t.elapsed());
    let decoded = latency_us.len();
    #[expect(clippy::cast_precision_loss, reason = "frame counts are small")]
    let fps = if run.is_zero() { 0.0 } else { decoded as f64 / run.as_secs_f64() };
    println!(
        "  first frame after {:.0} ms; {decoded} frames decoded in {:.2} s = {fps:.1} fps",
        first_frame.map_or(0.0, |t| t.saturating_duration_since(opened_at).as_secs_f64() * 1e3),
        run.as_secs_f64()
    );
    println!("  capture→decoded (host clock; loopback only): {}", quantiles(&mut latency_us));
    println!("  arrival gap: {}", quantiles(&mut gaps_us));
    println!(
        "  datagrams {}  fec-recovered {}  lost {}  nacks {}  refreshes {}  decode errors {}",
        stats.datagrams,
        stats.frames_fec,
        stats.frames_lost,
        stats.nacks,
        stats.refreshes,
        stats.decode_errors
    );
    println!("  audio packets {}  lost {}", stats.audio_packets, stats.audio_lost);
    println!("  quic paths: {}", link.paths());
    println!("  quic path (client side): {}", link.health());

    out.send(ClientMsg::Screen(ScreenRequest::Close(stream))).await?;
    drop(handle);
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(link);
    endpoint.close().await;
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
