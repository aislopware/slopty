//! Drawn frames timed from their capture to a paint, over a real connection to a worker on this
//! Mac: the measurement client of `tests/app/stream.rs` (`frame_time`).
//!
//! `slopty-glass <ip:port> <display> <scale> <seconds>` opens the display on the worker at that
//! scale and receives it as the app does (the link, the reassembler, VideoToolbox, the pacer),
//! painting on a 60 Hz beat in place of the app's window. Every frame is timed from the capture
//! stamp the worker drew it with, which on one machine is this process's clock too. It prints the
//! spreads as `MEASURE` lines and ends with one JSON line of what a test asserts on.
//!
//! It is a binary of its own, not code in the test, so that it runs from the harness's copy off
//! the repository volume ([`slopty_e2e::harness::bin_dir`]): its decoder session would otherwise
//! wait tens of seconds on the signature check.

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    glass::run().await
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod glass {
    use std::net::SocketAddr;
    use std::time::{Duration, Instant};

    use anyhow::{Context as _, bail};
    use slopty_client::pacing::{ClockAnchor, Pacer, percentile};
    use slopty_client::{LinkEvent, WorkerLink};
    use slopty_core::{ClientId, DisplayId};
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_proto::handshake::Hello;
    use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent, ScreenRequest};
    use slopty_proto::{ClientMsg, WorkerMsg};

    /// The display beat the frames are painted on: the drawn display's 60 Hz.
    const PAINT: Duration = Duration::from_micros(16_667);
    /// How long the stream may take to open.
    const OPEN: Duration = Duration::from_secs(60);
    /// The encoder's and the decoder's first second, which is not the path's.
    const WARM: Duration = Duration::from_secs(1);

    /// p50 / p95 / p99 / max of `samples`, in milliseconds.
    fn spread(samples: &mut [Duration]) -> String {
        samples.sort_unstable();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        format!(
            "p50 {:.2} / p95 {:.2} / p99 {:.2} / max {:.2} ms (n={})",
            ms(percentile(samples, 50)),
            ms(percentile(samples, 95)),
            ms(percentile(samples, 99)),
            ms(samples.last().copied().unwrap_or_default()),
            samples.len()
        )
    }

    #[expect(clippy::print_stdout, reason = "the measurement is what it prints")]
    #[expect(clippy::too_many_lines, reason = "one measurement, read top to bottom")]
    pub async fn run() -> anyhow::Result<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let [addr, display, scale, seconds] = args.as_slice() else {
            bail!("usage: slopty-glass <ip:port> <display> <scale> <seconds>");
        };
        let addr: SocketAddr = addr.parse().context("the worker's address")?;
        let display = DisplayId(display.parse().context("the display id")?);
        let scale: f32 = scale.parse().context("the scale")?;
        let run = Duration::from_secs(seconds.parse().context("the run length")?);

        let endpoint = bind_client()?;
        let client = ClientId::new();
        let hello = Hello { client, name: "glass".to_owned() };
        let conn = tokio::time::timeout(OPEN, connect_addr(&endpoint, addr, hello))
            .await
            .context("connect")??;
        let mut link = WorkerLink::start(conn);
        let mut events = link.events().context("the link's events")?;
        let quality = Quality { scale, ..Quality::default() };
        let target = CaptureTarget::Display(display);
        link.send(ClientMsg::Screen(ScreenRequest::Open { target, quality })).await?;
        let (stream, codec, width, height) = tokio::time::timeout(OPEN, async {
            loop {
                match events.recv().await {
                    Some(LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Opened {
                        stream,
                        codec,
                        width,
                        height,
                        ..
                    }))) => break Ok((stream, codec, width, height)),
                    Some(LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Closed {
                        reason,
                        ..
                    }))) => bail!("closed: {reason}"),
                    Some(_other) => {}
                    None => bail!("the link closed"),
                }
            }
        })
        .await
        .context("open")??;
        let handle = link.screen(stream, codec);
        let mut frames = handle.frames();
        // The link's other events are drained, as the app does, so its queue never fills.
        let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });

        let before = slopty_capture::host_now_us();
        let at = Instant::now();
        let after = slopty_capture::host_now_us();
        let clocks = ClockAnchor { at, host_us: before.midpoint(after) };
        let mut pacer = Pacer::default();
        pacer.share_clock(clocks);
        let mut paint = tokio::time::interval(PAINT);
        paint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let (mut to_arrival, mut to_decoded, mut to_glass) = (Vec::new(), Vec::new(), Vec::new());
        let warm = tokio::time::Instant::now().checked_add(WARM).context("the clock")?;
        let deadline = warm.checked_add(run).context("the run length")?;
        loop {
            tokio::select! {
                _tick = paint.tick() => {
                    if let Some(stamp) = pacer.painted() {
                        let shown = Instant::now();
                        pacer.shown(stamp, shown);
                        if tokio::time::Instant::now() > warm
                            && let Some(captured) = clocks.captured(stamp.pts_us)
                        {
                            to_glass.push(shown.saturating_duration_since(captured));
                        }
                    }
                }
                changed = frames.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let Some(frame) = frames.borrow_and_update().clone() else { continue };
                    let stamp = frame.stamp;
                    let _pace = pacer.offer(stamp);
                    if tokio::time::Instant::now() > warm
                        && let Some(captured) = clocks.captured(stamp.pts_us)
                    {
                        to_arrival.push(stamp.arrived.saturating_duration_since(captured));
                        to_decoded.push(stamp.decoded.saturating_duration_since(captured));
                    }
                }
                () = tokio::time::sleep_until(deadline) => break,
            }
        }
        let stats = handle.stats();
        let pacing = pacer.stats();
        let timed = to_glass.len();
        println!(
            "MEASURE glass, loopback QUIC, drawn display at {scale}: {width}×{height} {codec:?}, \
             60 Hz paint, {} s",
            run.as_secs()
        );
        println!("  capture → arrival:  {}", spread(&mut to_arrival));
        println!("  capture → decoded:  {}", spread(&mut to_decoded));
        println!("  capture → painted:  {}", spread(&mut to_glass));
        println!(
            "  client: decoded {} lost {} fec {} retransmitted {} nacks {} refreshes {} decode \
             errors {} | shown {} skipped {} repeats {} late {}",
            stats.frames,
            stats.frames_lost,
            stats.frames_fec,
            stats.frames_retransmit,
            stats.nacks,
            stats.refreshes,
            stats.decode_errors,
            pacing.presented,
            pacing.skipped,
            pacing.repeats,
            pacing.late
        );
        let verdict = serde_json::json!({
            "client": client.to_string(),
            "timed": timed,
            "frames_lost": stats.frames_lost,
            "refreshes": stats.refreshes,
            "decode_errors": stats.decode_errors,
        });
        println!("{verdict}");
        link.send(ClientMsg::Screen(ScreenRequest::Close(stream))).await?;
        drop(handle);
        drain.abort();
        link.close();
        endpoint.close(0_u32.into(), b"bye");
        let _drained = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
        Ok(())
    }
}
