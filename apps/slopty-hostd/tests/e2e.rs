//! ptyd + hostd + a client, all on this machine: pair, open a shell, see its output, close it.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::Duration;

    use slopty_client::LinkEvent;
    use slopty_core::ClientId;
    use slopty_net::client::{HostConn, bind_client, connect_with_ticket};
    use slopty_net::pairing::PairTicket;
    use slopty_net::{ClientMsg, HostMsg, Reach, SecretKey};
    use slopty_proto::PROTOCOL_VERSION;
    use slopty_proto::handshake::{Caps, ClientKind, Hello};
    use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent, ScreenRequest, SourceState};
    use slopty_proto::terminal::{OpenSession, TermEvent, TermRequest, TermSize};
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};

    const STEP: Duration = Duration::from_secs(20);

    /// A sibling binary from the same build. `cargo test -p slopty-hostd` on its own does not
    /// build ptyd, so build it on demand — into the profile directory this test binary came
    /// from, or a `--release` run would build a debug ptyd and then look for it beside the
    /// release hostd.
    fn bin(name: &str) -> PathBuf {
        let hostd = PathBuf::from(env!("CARGO_BIN_EXE_slopty-hostd"));
        let path = hostd.with_file_name(name);
        if !path.exists() {
            let release = hostd.parent().is_some_and(|dir| dir.ends_with("release"));
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let mut build = std::process::Command::new(cargo);
            build.args(["build", "-p", name]);
            if release {
                build.arg("--release");
            }
            let status = build.status().expect("run cargo");
            assert!(status.success(), "build {name}");
        }
        path
    }

    /// Keeps the daemons and the client endpoint alive for the test (dropping an iroh
    /// endpoint closes every connection on it).
    struct Guard(Vec<Child>, Option<slopty_net::Endpoint>);

    impl Drop for Guard {
        fn drop(&mut self) {
            for c in &mut self.0 {
                if let Ok(Some(status)) = c.try_wait() {
                    eprintln!("daemon pid {:?} already exited: {status}", c.id());
                }
                let _killed = c.start_kill();
            }
        }
    }

    /// Start ptyd and hostd in `dir`, pair a client, return the daemons and the connection.
    async fn connect(dir: &std::path::Path) -> (Guard, HostConn) {
        let (mut guard, ticket) = daemons(dir, Reach::Anywhere).await;
        let (endpoint, host) = dial(&ticket, Reach::Anywhere).await;
        guard.1 = Some(endpoint);
        (guard, host)
    }

    /// Start ptyd and hostd in `dir` and read the pairing ticket hostd prints.
    async fn daemons(dir: &std::path::Path, reach: Reach) -> (Guard, PairTicket) {
        let ptyd_sock = dir.join("ptyd.sock");
        let ctl_sock = dir.join("hostd.sock");
        let ptyd = Command::new(bin("slopty-ptyd"))
            .arg("--socket")
            .arg(&ptyd_sock)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("slopty-ptyd built alongside the tests");
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut hostd = Command::new(bin("slopty-hostd"));
        if reach.is_direct_only() {
            hostd.arg("--direct-only");
        }
        let mut hostd = hostd
            .arg("--ptyd-socket")
            .arg(&ptyd_sock)
            .arg("--ctl-socket")
            .arg(&ctl_sock)
            .arg("--data-dir")
            .arg(dir.join("data"))
            .arg("--print-ticket")
            // Any free port: the developer's own hostd may hold the default one.
            .arg("--port")
            .arg("0")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdout = hostd.stdout.take().unwrap();
        let guard = Guard(vec![ptyd, hostd], None);
        let mut line = String::new();
        tokio::time::timeout(STEP, BufReader::new(stdout).read_line(&mut line))
            .await
            .expect("hostd prints a ticket")
            .unwrap();
        let ticket: PairTicket = line.trim().parse().unwrap();
        (guard, ticket)
    }

    /// A fresh endpoint (new key, new client id) dialing `ticket`: a cold QUIC connection.
    async fn dial(ticket: &PairTicket, reach: Reach) -> (slopty_net::Endpoint, HostConn) {
        let endpoint = bind_client(SecretKey::generate(), reach).await.unwrap();
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            client: ClientId::new(),
            kind: ClientKind::Tool,
            name: "e2e".to_owned(),
            app_version: "0".to_owned(),
            caps: Caps::empty(),
            pair_token: None,
        };
        let host = tokio::time::timeout(STEP, connect_with_ticket(&endpoint, reach, ticket, hello))
            .await
            .unwrap()
            .unwrap();
        (endpoint, host)
    }

    /// Mint a fresh pairing ticket over hostd's control socket (tokens are single-use).
    async fn mint(ctl_sock: &std::path::Path) -> PairTicket {
        use tokio::io::AsyncWriteExt as _;
        let stream = tokio::net::UnixStream::connect(ctl_sock).await.unwrap();
        let (rd, mut wr) = stream.into_split();
        let mut line = serde_json::to_vec(&slopty_host::ctl::CtlRequest::Ticket).unwrap();
        line.push(b'\n');
        wr.write_all(&line).await.unwrap();
        wr.shutdown().await.unwrap();
        let mut reply = String::new();
        BufReader::new(rd).read_line(&mut reply).await.unwrap();
        match serde_json::from_str(reply.trim()).unwrap() {
            slopty_host::ctl::CtlReply::Ticket { ticket } => ticket.parse().unwrap(),
            other => panic!("unexpected ctl reply: {other:?}"),
        }
    }

    #[tokio::test]
    async fn shell_round_trip_over_iroh() {
        let dir = tempfile::tempdir().unwrap();
        let (_guard, mut host) = connect(dir.path()).await;
        assert_eq!(host.ack.protocol, PROTOCOL_VERSION);
        assert!(host.ack.sessions.is_empty());

        let size = TermSize { cols: 40, rows: 6, ..TermSize::default() };
        host.tx
            .send(&ClientMsg::OpenSession(OpenSession {
                size,
                cwd: None,
                command: vec!["/bin/sh".to_owned()],
                env: vec![("PS1".to_owned(), "$ ".to_owned())],
                title: None,
                attach: true,
            }))
            .await
            .unwrap();
        // The canvas snapshot (sent right after HelloAck) and the canvas delta for the new
        // terminal item interleave with SessionOpened on the control stream; skip them.
        let session = loop {
            match tokio::time::timeout(STEP, host.rx.recv()).await.unwrap().unwrap() {
                HostMsg::SessionOpened(summary) => break summary.id,
                HostMsg::Canvas(_sync) => {}
                other => panic!("unexpected control message before SessionOpened: {other:?}"),
            }
        };
        let (header, mut events) =
            tokio::time::timeout(STEP, host.accept_session_stream()).await.unwrap().unwrap();
        assert_eq!(header.session, session);

        host.tx
            .send(&ClientMsg::Term {
                session,
                req: TermRequest::Raw(b"echo marker-$((40+2))\n".to_vec()),
            })
            .await
            .unwrap();
        let deadline = tokio::time::Instant::now() + STEP;
        let mut saw_driver = false;
        loop {
            let ev = tokio::time::timeout_at(deadline, events.recv()).await.unwrap().unwrap();
            match ev {
                TermEvent::Driver { you } => saw_driver = you,
                TermEvent::Frame(f) => {
                    assert_eq!((f.cols, f.rows), (40, 6));
                    if f.updates.iter().any(|u| u.line.text().contains("marker-42")) {
                        break;
                    }
                }
                _other => {}
            }
        }
        assert!(saw_driver, "first attached client drives the size");

        host.tx.send(&ClientMsg::Term { session, req: TermRequest::Close }).await.unwrap();
        loop {
            match tokio::time::timeout(STEP, host.rx.recv()).await.unwrap().unwrap() {
                HostMsg::SessionClosed { session: s, .. } => {
                    assert_eq!(s, session);
                    break;
                }
                _other => {}
            }
        }
        host.tx.send(&ClientMsg::Ping { sent_at: slopty_core::MonoTime::now() }).await.unwrap();
        loop {
            match tokio::time::timeout(STEP, host.rx.recv()).await.unwrap().unwrap() {
                HostMsg::Pong { .. } => break,
                HostMsg::Canvas(_sync) => {}
                other => panic!("unexpected control message before Pong: {other:?}"),
            }
        }
    }

    /// Streams the first display through hostd into the real client stack (`HostLink` +
    /// `ScreenHandle`): reassembly, NACK/report traffic and hardware decode all run as the app
    /// would run them. Needs Screen Recording permission for the test process, so it only runs
    /// when `SLOPTY_SCREEN_E2E=1`.
    #[tokio::test]
    async fn screen_stream_over_iroh() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (_guard, host) = connect(dir.path()).await;
        let mut link = slopty_client::HostLink::start(host);
        let mut events = link.events().unwrap();

        link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let display = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Listing { displays, windows })) => {
                    eprintln!("{} windows, {} displays", windows.len(), displays.len());
                    break displays.first().expect("a display").id;
                }
                LinkEvent::Control(HostMsg::Canvas(_sync)) => {}
                other => panic!("unexpected event before Listing: {other:?}"),
            }
        };

        let quality = Quality { fps: 60, bitrate_bps: 8_000_000, scale: 0.5, ..Quality::default() };
        let target = CaptureTarget::Display(display);
        link.send(ClientMsg::Screen(ScreenRequest::Open { target, quality })).await.unwrap();
        let (stream, codec, width, height) = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Opened {
                    stream,
                    codec,
                    width,
                    height,
                    ..
                })) => break (stream, codec, width, height),
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Closed { reason, .. })) => {
                    panic!("open failed: {reason}")
                }
                LinkEvent::Control(HostMsg::Canvas(_sync)) => {}
                other => panic!("unexpected event before Opened: {other:?}"),
            }
        };
        eprintln!("opened {stream} {codec:?} {width}x{height}");

        let screen = link.screen(stream, codec);
        let mut frames = screen.frames();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut decoded = 0_u32;
        while tokio::time::timeout_at(deadline, frames.changed()).await.is_ok_and(|r| r.is_ok()) {
            let frame = frames.borrow_and_update().clone().expect("a frame after a change");
            assert_eq!(
                (frame.frame.image.width(), frame.frame.image.height()),
                (width as usize, height as usize)
            );
            decoded += 1;
        }
        let stats = screen.stats();
        eprintln!("decoded {decoded}, cursor {:?}, {stats:?}", *screen.cursor().borrow());
        assert!(decoded >= 10, "expected a steady stream, got {decoded} decoded frames");
        assert!(stats.frames >= u64::from(decoded), "{stats:?}");
        assert_eq!(stats.decode_errors, 0, "{stats:?}");
        assert_eq!(stats.frames_lost, 0, "{stats:?}");
        drop(screen);

        link.send(ClientMsg::Screen(ScreenRequest::Close(stream))).await.unwrap();
        loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Closed { stream: s, .. })) => {
                    assert_eq!(s, stream);
                    break;
                }
                _other => {}
            }
        }
        link.close();
    }

    /// One start-up sample: what the first seconds of a stream on a cold connection looked like.
    #[derive(Debug, Default)]
    struct StartUp {
        /// `Open` sent → `Opened` received.
        opened_ms: f64,
        /// `Open` sent → first datagram of the stream.
        first_datagram_ms: f64,
        /// `Open` sent → first frame complete (reassembled, handed to the decoder).
        first_frame_ms: f64,
        /// `Open` sent → first decoded picture.
        first_decoded_ms: f64,
        /// Longest first-fragment → complete wait of any frame (the keyframe's wire spread).
        hold_max_ms: f64,
        decoded: u32,
        gap_p50_ms: f64,
        gap_p90_ms: f64,
        gap_max_ms: f64,
        stalls: u64,
        stalled_ms: u64,
        nacks: u64,
        refreshes: u64,
        lost: u64,
        /// The host's bitrate decisions: `target(verdict)`.
        rate: String,
        /// What the presentation path did with the stream: the same `Pacer` the GPUI element
        /// runs, fed the same `frames` channel and paced by a 60 Hz timer standing in for the
        /// display link.
        pacing: slopty_client::PacingStats,
    }

    /// `min / p50 / p90 / max` of `samples`, in milliseconds.
    fn quantiles(samples: &mut [f64]) -> (f64, f64, f64) {
        samples.sort_by(f64::total_cmp);
        let at = |q: f64| {
            let last = samples.len().saturating_sub(1);
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "an index below 2^53"
            )]
            let i = ((last as f64) * q).round() as usize;
            samples.get(i.min(last)).copied().unwrap_or(0.0)
        };
        (at(0.5), at(0.9), at(1.0))
    }

    fn ms(from: std::time::Instant, to: Option<std::time::Instant>) -> f64 {
        to.map_or(f64::NAN, |t| t.saturating_duration_since(from).as_secs_f64() * 1e3)
    }

    /// Stream the first display on a cold connection for `seconds` and measure the start.
    async fn start_up_sample(ctl_sock: &std::path::Path, seconds: u64) -> StartUp {
        let ticket = mint(ctl_sock).await;
        let (endpoint, host) = dial(&ticket, Reach::DirectOnly).await;
        let mut link = slopty_client::HostLink::start(host);
        let mut events = link.events().unwrap();
        link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let display = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Listing { displays, .. })) => {
                    break displays.first().expect("a display").id;
                }
                _other => {}
            }
        };
        let target = CaptureTarget::Display(display);
        let quality = Quality::default();
        let sent_at = std::time::Instant::now();
        link.send(ClientMsg::Screen(ScreenRequest::Open { target, quality })).await.unwrap();
        let (stream, codec) = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Opened {
                    stream, codec, ..
                })) => {
                    break (stream, codec);
                }
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Closed { reason, .. })) => {
                    panic!("open failed: {reason}")
                }
                _other => {}
            }
        };
        let mut sample = StartUp {
            opened_ms: ms(sent_at, Some(std::time::Instant::now())),
            ..StartUp::default()
        };
        let screen = link.screen(stream, codec);
        let mut frames = screen.frames();
        let deadline =
            tokio::time::Instant::now().checked_add(Duration::from_secs(seconds)).unwrap();
        let mut gaps: Vec<f64> = Vec::new();
        let mut last: Option<std::time::Instant> = None;
        let mut rate: Vec<String> = Vec::new();
        // The app's presentation path: the element's pacer, offered every decoded frame and
        // asked to present on a 60 Hz timer. A tokio timer is not a display link, so the
        // interval jitter carries the timer's own; arrival -> present does not.
        let mut pacer = slopty_client::Pacer::default();
        let mut paint = tokio::time::interval(Duration::from_micros(16_667));
        paint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _paint = paint.tick() => pacer.presented(),
                changed = frames.changed() => {
                    if changed.is_err() { break; }
                    let Some(frame) = frames.borrow_and_update().clone() else { continue };
                    let _pace = pacer.offer(frame.stamp);
                    let now = std::time::Instant::now();
                    if let Some(prev) = last {
                        gaps.push(now.saturating_duration_since(prev).as_secs_f64() * 1e3);
                    }
                    last = Some(now);
                    sample.decoded = sample.decoded.saturating_add(1);
                }
                () = tokio::time::sleep_until(deadline) => break,
                ev = events.recv() => match ev {
                    Some(LinkEvent::Control(HostMsg::Screen(ScreenEvent::Rate { target_bps, verdict, capped, .. }))) => {
                        rate.push(format!("{:.1}({verdict:?}{})", f64::from(target_bps) / 1e6, if capped { "/cwnd" } else { "" }));
                    }
                    Some(LinkEvent::Disconnected(why)) => panic!("disconnected: {why}"),
                    Some(_other) => {}
                    None => break,
                },
            }
        }
        let stats = screen.stats();
        sample.first_datagram_ms = ms(sent_at, stats.first_datagram_at);
        sample.first_frame_ms = ms(sent_at, stats.first_frame_at);
        sample.first_decoded_ms = ms(sent_at, stats.first_decoded_at);
        sample.hold_max_ms = stats.hold_max.as_secs_f64() * 1e3;
        (sample.gap_p50_ms, sample.gap_p90_ms, sample.gap_max_ms) = quantiles(&mut gaps);
        sample.stalls = stats.stalls;
        sample.stalled_ms = stats.stalled_ms;
        sample.nacks = stats.nacks;
        sample.refreshes = stats.refreshes;
        sample.lost = stats.frames_lost;
        sample.rate = rate.join(" ");
        sample.pacing = pacer.stats();
        assert_eq!(stats.decode_errors, 0, "{stats:?}");
        drop(screen);
        link.send(ClientMsg::Screen(ScreenRequest::Close(stream))).await.unwrap();
        loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Closed { stream: s, .. }))
                    if s == stream =>
                {
                    break;
                }
                _other => {}
            }
        }
        link.close();
        endpoint.close().await;
        sample
    }

    /// One row of the loss table: what a run at a given injected drop rate delivered.
    #[derive(Debug, Default)]
    struct LossRow {
        drop_permille: u32,
        /// Frames handed to the decoder.
        frames: u64,
        /// Pictures the decoder gave back and published for the UI.
        decoded: u64,
        /// Frames the decoder rejected.
        decode_errors: u64,
        /// Of those, the ones that needed parity.
        fec: u64,
        /// Of those, the ones that needed a retransmission.
        retransmit: u64,
        /// Frames given up on.
        lost: u64,
        /// Stalls the receiver saw (the link holding datagrams, then releasing them together).
        /// On a busy machine loopback stalls, and a stalled run says nothing about loss policy.
        stalls: u64,
        nacks: u64,
        refreshes: u64,
        /// Datagrams the receiver saw, and the data fragments that never arrived.
        datagrams: u64,
        datagrams_lost: u64,
        /// Bytes those datagrams carried, parity included: what the parity policy costs.
        bytes: u64,
        /// Parity fragments per thousand data fragments, as seen on the wire: what the
        /// packetizer's rounding actually costs at the ratio the controller settled on.
        parity_seen: u16,
        /// The frame layouts behind that ratio. `data_shards / frames` is how many fragments a
        /// frame took, and `1000 * frames / data_shards` is what the old rule — at least one
        /// parity fragment per frame, whatever the ratio — would have put on the wire for the
        /// very same frames, which is the only fair before/after when the desktop's content is
        /// not under the test's control.
        data_shards: u64,
        parity_shards: u64,
        /// Gaps between decoded frames.
        gap_p50_ms: f64,
        gap_p90_ms: f64,
        gap_max_ms: f64,
    }

    /// Stream the first display for `seconds` with `drop_permille` of the client's datagrams
    /// thrown away before the router sees them, and report what got through.
    async fn loss_sample(ctl_sock: &std::path::Path, seconds: u64, drop_permille: u32) -> LossRow {
        let ticket = mint(ctl_sock).await;
        let (endpoint, host) = dial(&ticket, Reach::DirectOnly).await;
        let mut link = slopty_client::HostLink::start(host);
        // Deterministic: the same rate always drops the same datagrams of the sequence.
        link.screens().set_loss(drop_permille);
        let mut events = link.events().unwrap();
        link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let display = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Listing { displays, .. })) => {
                    break displays.first().expect("a display").id;
                }
                _other => {}
            }
        };
        let target = CaptureTarget::Display(display);
        link.send(ClientMsg::Screen(ScreenRequest::Open { target, quality: Quality::default() }))
            .await
            .unwrap();
        let (stream, codec) = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Opened {
                    stream, codec, ..
                })) => break (stream, codec),
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Closed { reason, .. })) => {
                    panic!("open failed: {reason}")
                }
                _other => {}
            }
        };
        let screen = link.screen(stream, codec);
        let mut frames = screen.frames();
        let deadline =
            tokio::time::Instant::now().checked_add(Duration::from_secs(seconds)).unwrap();
        let mut gaps: Vec<f64> = Vec::new();
        let mut last: Option<std::time::Instant> = None;
        let mut row = LossRow { drop_permille, ..LossRow::default() };
        loop {
            tokio::select! {
                changed = frames.changed() => {
                    if changed.is_err() { break; }
                    if frames.borrow_and_update().is_none() { continue; }
                    row.decoded = row.decoded.saturating_add(1);
                    let now = std::time::Instant::now();
                    if let Some(prev) = last {
                        gaps.push(now.saturating_duration_since(prev).as_secs_f64() * 1e3);
                    }
                    last = Some(now);
                }
                () = tokio::time::sleep_until(deadline) => break,
                ev = events.recv() => match ev {
                    Some(LinkEvent::Disconnected(why)) => panic!("disconnected: {why}"),
                    Some(_other) => {}
                    None => break,
                },
            }
        }
        let stats = screen.stats();
        row.frames = stats.frames;
        row.decode_errors = stats.decode_errors;
        row.fec = stats.frames_fec;
        row.retransmit = stats.frames_retransmit;
        row.lost = stats.frames_lost;
        row.stalls = stats.stalls;
        row.nacks = stats.nacks;
        row.refreshes = stats.refreshes;
        row.datagrams = stats.datagrams;
        row.datagrams_lost = stats.datagrams_lost;
        row.bytes = stats.bytes;
        row.parity_seen = stats.parity_permille;
        row.data_shards = stats.data_shards;
        row.parity_shards = stats.parity_shards;
        (row.gap_p50_ms, row.gap_p90_ms, row.gap_max_ms) = quantiles(&mut gaps);
        drop(screen);
        link.send(ClientMsg::Screen(ScreenRequest::Close(stream))).await.unwrap();
        link.close();
        endpoint.close().await;
        row
    }

    /// What parity, NACK and refresh recover on a path that drops datagrams, at three rates.
    /// Loopback carries everything, so the loss is injected on the client's receive path with a
    /// fixed seed: the same rate always drops the same datagrams, and two builds compare on the
    /// same losses. `SLOPTY_E2E_SECONDS` (default 5) per rate. Prints a table for
    /// `docs/MEASUREMENTS.md`.
    #[tokio::test(flavor = "multi_thread")]
    async fn screen_under_injected_loss() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let seconds: u64 =
            std::env::var("SLOPTY_E2E_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
        let dir = tempfile::tempdir().unwrap();
        let _logs = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .try_init();
        slopty_client::warm_up_decoder();
        let (_guard, _ticket) = daemons(dir.path(), Reach::DirectOnly).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let ctl_sock = dir.path().join("hostd.sock");
        let mut rows = Vec::new();
        // 0/20/50 ‰ are the rates the ruling is about; 100 ‰ is the stress row that shows
        // what happens once parity alone cannot cover the loss.
        for permille in [0_u32, 20, 50, 100] {
            rows.push(loss_sample(&ctl_sock, seconds, permille).await);
        }
        eprintln!(
            "| drop | frames | by parity | by nack | lost | nack / refresh | datagrams (lost) | kB (B/frame) | fragments/frame | parity seen (one-per-frame would be) | stalls | gap p50 / p90 / max |"
        );
        for r in &rows {
            eprintln!(
                "| {} ‰ | {} ({} decoded) | {} | {} | {} | {} / {} | {} ({}) | {} ({}) | {} | {} ‰ ({} ‰) | {} | {:.1} / {:.1} / {:.1} ms |",
                r.drop_permille,
                r.frames,
                r.decoded,
                r.fec,
                r.retransmit,
                r.lost,
                r.nacks,
                r.refreshes,
                r.datagrams,
                r.datagrams_lost,
                r.bytes / 1000,
                r.bytes / r.frames.max(1),
                r.data_shards / r.frames.max(1),
                r.parity_seen,
                1000 * r.frames / r.data_shards.max(1),
                r.stalls,
                r.gap_p50_ms,
                r.gap_p90_ms,
                r.gap_max_ms,
            );
        }
        for r in &rows {
            assert!(r.frames >= 1, "{} permille reassembled nothing: {r:?}", r.drop_permille);
            // The decoder has to have produced pictures, not merely been fed: a stream where
            // every decode fails reassembles exactly as well as one that works.
            assert!(r.decoded >= 1, "{} permille decoded nothing: {r:?}", r.drop_permille);
            assert_eq!(
                r.decode_errors, 0,
                "{} permille had decoder rejections: {r:?}",
                r.drop_permille
            );
        }
        // `slopty_media::Config::max_hold`, the longest an incomplete frame is held.
        // The verdicts are about the loss policy, so the run has to be one where the machine
        // kept up — and the only evidence of that worth having is at the datagram level. The
        // gaps between *decoded* frames say nothing: a still desktop draws 8 fps because
        // nothing is changing, and keying the guard on them would let a broken recovery that
        // starves decode while datagrams keep arriving skip every verdict below. A stall is
        // the datagram-level signal: host and client share this process, so nothing but the
        // scheduler can hold a loopback datagram for a stall gap. Since the send stamps
        // stopped charging quiet sources it fires rarely — an idle machine reports none —
        // where the same skip on the old stall count fired on nearly every run.
        let stalled: Vec<u64> = rows.iter().map(|r| r.stalls).filter(|s| *s > 0).collect();
        if !stalled.is_empty() {
            eprintln!(
                "{stalled:?} stalls on loopback: the scheduler held datagrams, not the link; \
                 verdicts skipped"
            );
            return;
        }
        let clean = rows.first().expect("the 0 permille row");
        assert_eq!(clean.lost, 0, "a lossless path lost frames: {clean:?}");
        assert_eq!(clean.refreshes, 0, "a lossless path needed a refresh: {clean:?}");
        assert_eq!(clean.nacks, 0, "a lossless path asked for a retransmission: {clean:?}");
        // Parity has to answer the loss: on a lossy path it must be repairing frames, and
        // between them parity, NACK and refresh must not leave a frame behind at these rates.
        for r in rows.iter().skip(1) {
            assert!(r.fec > 0, "{} permille loss repaired nothing: {r:?}", r.drop_permille);
            // Not zero: a frame cut into two fragments plus one parity is beyond repair when
            // two of its three datagrams go, which at 50 ‰ happens to roughly one frame in a
            // few hundred however good the policy is. What must hold is that the *visible*
            // loss stays rare, since every lost frame is a refresh and every refresh a hitch.
            assert!(
                r.lost.saturating_mul(100) <= r.frames,
                "{} permille loss lost more than one frame in a hundred: {r:?}",
                r.drop_permille
            );
        }
    }

    /// Start-up on a cold connection, several samples in one run: how long the first frame
    /// takes and whether the first seconds stall. `SLOPTY_E2E_SAMPLES` (default 5) samples of
    /// `SLOPTY_E2E_SECONDS` (default 3) each, every one on a fresh QUIC connection to the same
    /// hostd, native scale at the default quality (what the app opens). Prints a table; the
    /// numbers go to `docs/MEASUREMENTS.md`.
    #[tokio::test(flavor = "multi_thread")]
    async fn screen_start_up_over_iroh() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let samples: u32 =
            std::env::var("SLOPTY_E2E_SAMPLES").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
        let seconds: u64 =
            std::env::var("SLOPTY_E2E_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
        let dir = tempfile::tempdir().unwrap();
        let _logs = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .try_init();
        // The app warms the decoder up at launch, long before it dials a host.
        slopty_client::warm_up_decoder();
        let (_guard, _ticket) = daemons(dir.path(), Reach::DirectOnly).await;
        // The daemon warms ScreenCaptureKit up right after it is online; by the time a user
        // opens a window that has long finished, so let it finish here too.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let ctl_sock = dir.path().join("hostd.sock");
        let mut rows = Vec::new();
        for i in 0..samples {
            let s = start_up_sample(&ctl_sock, seconds).await;
            eprintln!(
                "| {i} | {:.0} | {:.0} | {:.0} | {:.0} | {:.0} | {} | {:.1} / {:.1} / {:.1} | {} ({} ms) | {} / {} / {} | {} |",
                s.opened_ms,
                s.first_datagram_ms,
                s.first_frame_ms,
                s.first_decoded_ms,
                s.hold_max_ms,
                s.decoded,
                s.gap_p50_ms,
                s.gap_p90_ms,
                s.gap_max_ms,
                s.stalls,
                s.stalled_ms,
                s.nacks,
                s.refreshes,
                s.lost,
                s.rate
            );
            rows.push(s);
        }
        eprintln!(
            "| sample | opened | first datagram | first frame | first decoded | hold max | decoded | gap p50 / p90 / max | stalls (stalled) | nack / refresh / lost | host target |"
        );
        for (i, s) in rows.iter().enumerate() {
            eprintln!(
                "| {i} | {:.0} ms | {:.0} ms | {:.0} ms | {:.0} ms | {:.0} ms | {} | {:.1} / {:.1} / {:.1} ms | {} ({} ms) | {} / {} / {} | {} |",
                s.opened_ms,
                s.first_datagram_ms,
                s.first_frame_ms,
                s.first_decoded_ms,
                s.hold_max_ms,
                s.decoded,
                s.gap_p50_ms,
                s.gap_p90_ms,
                s.gap_max_ms,
                s.stalls,
                s.stalled_ms,
                s.nacks,
                s.refreshes,
                s.lost,
                s.rate
            );
        }
        eprintln!(
            "| sample | presented | arrival → present p50 / p95 / max | decode p50 | present every | jitter | skip / repeat / late |"
        );
        for (i, s) in rows.iter().enumerate() {
            let p = &s.pacing;
            let ms = |d: Duration| d.as_secs_f64() * 1e3;
            eprintln!(
                "| {i} | {} | {:.1} / {:.1} / {:.1} ms | {:.1} ms | {:.1} ms | ±{:.1} ms | {} / {} / {} |",
                p.presented,
                ms(p.latency_p50),
                ms(p.latency_p95),
                ms(p.latency_max),
                ms(p.decode_p50),
                ms(p.interval_p50),
                ms(p.interval_jitter),
                p.skipped,
                p.repeats,
                p.late,
            );
        }
        for (i, s) in rows.iter().enumerate() {
            assert!(s.decoded >= 1, "sample {i} decoded nothing: {s:?}");
            assert_eq!(s.lost, 0, "sample {i} lost frames: {s:?}");
            // Present on arrival: a decoded frame is never held back for a later paint, so no
            // frame waits longer than the decode plus one paint interval plus slack.
            assert!(
                s.pacing.presented > 0 && s.pacing.late == 0,
                "sample {i} presented nothing, or presented out of order: {:?}",
                s.pacing
            );
        }
    }

    // ---- the path-flap harness ------------------------------------------------------------
    //
    // DECISIONS "Path flap under investigation": a connection went direct → relay-only for 43 s
    // → direct while the machine was compiling, which is a 50× latency cliff. iroh always
    // prefers a live direct path, so the direct path must have been *closed*. This drives the
    // load a `cargo build` applies — all-core CPU, the same at a raised thread QoS, and memory
    // plus I/O — at a real connection that holds both a direct and a relay path, and reads back
    // what the transport did.

    /// A load shape applied while the harness watches the connection.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Shape {
        /// Every core spinning at the default quality of service.
        Cpu,
        /// The same, at `USER_INITIATED`: threads that outrank a default-QoS worker.
        CpuUserInitiated,
        /// Gigabytes written and read back, plus many small files: what a linker does.
        MemoryIo,
    }

    impl Shape {
        const fn name(self) -> &'static str {
            match self {
                Self::Cpu => "cpu",
                Self::CpuUserInitiated => "cpu-user-initiated",
                Self::MemoryIo => "memory-io",
            }
        }
    }

    /// Threads applying a [`Shape`]; they stop when this is dropped.
    struct Load {
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        threads: Vec<std::thread::JoinHandle<()>>,
    }

    impl Load {
        fn start(shape: Shape, scratch: &std::path::Path) -> Self {
            use std::sync::atomic::AtomicBool;

            let stop = std::sync::Arc::new(AtomicBool::new(false));
            let cores = std::thread::available_parallelism().map_or(8, std::num::NonZero::get);
            let mut threads = Vec::new();
            for worker in 0..cores {
                let stop = std::sync::Arc::clone(&stop);
                let scratch = scratch.to_path_buf();
                threads.push(std::thread::spawn(move || match shape {
                    Shape::Cpu => burn(&stop, false),
                    Shape::CpuUserInitiated => burn(&stop, true),
                    Shape::MemoryIo => churn(&stop, &scratch, worker),
                }));
            }
            Self { stop, threads }
        }
    }

    impl Drop for Load {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            for t in self.threads.drain(..) {
                let _joined = t.join();
            }
        }
    }

    /// Spin until told to stop, optionally after asking the scheduler to treat this thread as
    /// user-initiated work — the class Xcode's and cargo's build threads run at.
    fn burn(stop: &std::sync::atomic::AtomicBool, user_initiated: bool) {
        if user_initiated {
            // SAFETY: `pthread_set_qos_class_self_np` takes a QoS class and a relative priority
            // and only ever affects the calling thread (Apple's Energy Efficiency Guide, "Set
            // Quality of Service"); it borrows nothing.
            let set = unsafe {
                libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0)
            };
            assert_eq!(set, 0, "pthread_set_qos_class_self_np");
        }
        let mut x = 0x9E37_79B9_7F4A_7C15_u64;
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            for _ in 0..4_096 {
                x = x
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                std::hint::black_box(x);
            }
        }
    }

    /// Write and read back gigabytes, and churn many small files, until told to stop.
    fn churn(stop: &std::sync::atomic::AtomicBool, scratch: &std::path::Path, worker: usize) {
        use std::io::{Read as _, Write as _};

        let dir = scratch.join(format!("churn-{worker}"));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let block = vec![0x5A_u8; 8 << 20];
        let mut round = 0_u64;
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            let big = dir.join(format!("big-{round}"));
            if let Ok(mut f) = std::fs::File::create(&big) {
                // 512 MB a round, so a 90 s run moves several gigabytes per thread.
                for _ in 0..64 {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let _written = f.write_all(&block);
                }
                let _flushed = f.sync_all();
            }
            if let Ok(mut f) = std::fs::File::open(&big) {
                let mut sink = vec![0_u8; 8 << 20];
                while let Ok(n) = f.read(&mut sink) {
                    if n == 0 || stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                }
            }
            let _removed = std::fs::remove_file(&big);
            // The many-small-files half: metadata pressure, not throughput.
            for i in 0..2_000 {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let small = dir.join(format!("small-{i}"));
                let _written = std::fs::write(&small, b"slopty");
                let _removed = std::fs::remove_file(&small);
            }
            // A few hundred megabytes touched and dropped: memory pressure on top of the I/O.
            let mut hot = vec![0_u8; 256 << 20];
            for page in hot.chunks_mut(4_096) {
                if let Some(first) = page.first_mut() {
                    *first = 1;
                }
            }
            std::hint::black_box(&hot);
            drop(hot);
            round = round.wrapping_add(1);
        }
        let _cleaned = std::fs::remove_dir_all(&dir);
    }

    /// Tees the test's own log to stderr and to a buffer, so the harness can read back what
    /// noq said about a path it abandoned (`iroh::_events::path`, which carries the reason).
    #[derive(Clone)]
    struct Tee(std::sync::Arc<parking_lot::Mutex<Vec<u8>>>);

    impl std::io::Write for Tee {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(buf);
            std::io::stderr().write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            std::io::stderr().flush()
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for Tee {
        type Writer = Self;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    /// What one load shape did to the connection.
    #[derive(Debug)]
    struct FlapRow {
        shape: &'static str,
        /// Times the selected path went from direct to relayed.
        to_relay: u32,
        /// Longest unbroken stretch on a relayed path, seconds.
        relay_max_s: f64,
        /// Seconds on a relayed path in total.
        relay_total_s: f64,
        /// Worst round trip seen on the selected path.
        rtt_max_ms: f64,
        /// Round trip at the end, for scale.
        rtt_last_ms: f64,
        /// Lines the transport logged about a closed path (the abandon reason lives here).
        closed: Vec<String>,
        /// Receiver stalls on the display stream over the run.
        stalls: u64,
        /// Frames the display stream delivered.
        frames: u64,
        samples: u32,
    }

    /// Drive a real connection that holds both a direct and a relay path under the load a build
    /// applies, and read back whether the direct path is ever closed.
    ///
    /// Gated on `SLOPTY_FLAP_E2E`, `SLOPTY_FLAP_SECONDS` for the length of the load (default 90).
    /// Needs the default relay map: with no relay path there is nothing to flap onto, and the
    /// case says so and skips. One case per load shape, and nextest is told to give each of them
    /// every thread — a shape saturates the machine, so two at once would measure each other.
    #[tokio::test(flavor = "multi_thread")]
    async fn path_flap_under_cpu_load() {
        flap_case(Shape::Cpu).await;
    }

    /// The same saturation from threads that asked the scheduler to be treated as interactive,
    /// which is what a build's own workers do.
    #[tokio::test(flavor = "multi_thread")]
    async fn path_flap_under_user_initiated_cpu_load() {
        flap_case(Shape::CpuUserInitiated).await;
    }

    /// Memory and I/O rather than CPU: the other half of what a build does to a machine.
    #[tokio::test(flavor = "multi_thread")]
    async fn path_flap_under_memory_io_load() {
        flap_case(Shape::MemoryIo).await;
    }

    /// Run one shape end to end and print its verdict line.
    async fn flap_case(shape: Shape) {
        if std::env::var_os("SLOPTY_FLAP_E2E").is_none() {
            eprintln!("SLOPTY_FLAP_E2E unset; skipping");
            return;
        }
        let seconds: u64 =
            std::env::var("SLOPTY_FLAP_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(90);
        let log = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let _logs = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(
                |_unset| {
                    // noq's own path events carry the abandon reason.
                    tracing_subscriber::EnvFilter::new("info,iroh::_events::path=debug")
                },
            ))
            .with_writer(Tee(std::sync::Arc::clone(&log)))
            .try_init();
        let dir = tempfile::tempdir().unwrap();
        // Relays on: both ends must hold a relay path as well as the loopback direct one.
        let (_guard, ticket) = daemons(dir.path(), Reach::Anywhere).await;
        let started = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let Some(r) = flap_sample(&ticket, dir.path(), shape, seconds, &log).await else {
            eprintln!("{}: no relay path (relay unreachable?); skipping", shape.name());
            return;
        };
        eprintln!(
            "flap {} ran {started} .. {}",
            shape.name(),
            chrono::Local::now().format("%H:%M:%S")
        );
        eprintln!(
            "| {} | {} to relay | relay {:.1} s longest, {:.1} s total | rtt {:.1} ms worst, {:.1} ms last | {} closed | {} stalls | {} frames | {} samples |",
            r.shape,
            if r.to_relay == 0 { "none".to_owned() } else { r.to_relay.to_string() },
            r.relay_max_s,
            r.relay_total_s,
            r.rtt_max_ms,
            r.rtt_last_ms,
            if r.closed.is_empty() { "none".to_owned() } else { r.closed.len().to_string() },
            r.stalls,
            r.frames,
            r.samples,
        );
        for line in &r.closed {
            eprintln!("    {line}");
        }
        assert!(r.samples > 0, "{} sampled nothing: {r:?}", r.shape);
        assert!(r.frames > 0, "{} streamed nothing: {r:?}", r.shape);
        // The verdict this harness exists for. A direct path that closes under load is the flap;
        // the reason lines above say why, and the ruling follows them.
        assert_eq!(r.to_relay, 0, "{} pushed the connection onto a relay: {r:?}", r.shape);
    }

    /// One shape: a fresh connection, a terminal and a display stream, the load, and a sample
    /// of the selected path every 250 ms. `None` when the connection never held a relay path,
    /// which means there was nothing to flap onto.
    async fn flap_sample(
        ticket: &PairTicket,
        scratch: &std::path::Path,
        shape: Shape,
        seconds: u64,
        log: &std::sync::Arc<parking_lot::Mutex<Vec<u8>>>,
    ) -> Option<FlapRow> {
        let (endpoint, host) = dial(ticket, Reach::Anywhere).await;
        let mut link = slopty_client::HostLink::start(host);
        let mut events = link.events().unwrap();
        // A terminal, so the control and session streams carry traffic too.
        link.send(ClientMsg::OpenSession(OpenSession {
            size: TermSize { cols: 80, rows: 24, ..TermSize::default() },
            cwd: None,
            command: vec!["/bin/sh".to_owned()],
            env: vec![("PS1".to_owned(), "$ ".to_owned())],
            title: None,
            attach: true,
        }))
        .await
        .unwrap();
        // A display stream, so datagrams flow the whole time.
        link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let display = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Listing { displays, .. })) => {
                    break displays.first().expect("a display").id;
                }
                _other => {}
            }
        };
        link.send(ClientMsg::Screen(ScreenRequest::Open {
            target: CaptureTarget::Display(display),
            quality: Quality::default(),
        }))
        .await
        .unwrap();
        let (stream, codec) = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Opened {
                    stream, codec, ..
                })) => break (stream, codec),
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Closed { reason, .. })) => {
                    panic!("open failed: {reason}")
                }
                _other => {}
            }
        };
        let screen = link.screen(stream, codec);
        // Hole punching and the relay handshake both need a moment before the picture is fair.
        tokio::time::sleep(Duration::from_secs(3)).await;
        if !link.paths().contains("relay") {
            drop(screen);
            link.close();
            endpoint.close().await;
            return None;
        }
        eprintln!("flap {}: paths before load: {}", shape.name(), link.paths());
        let mark = log.lock().len();
        let load = Load::start(shape, scratch);

        let step = Duration::from_millis(250);
        let mut ticks = tokio::time::interval(step);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_secs(seconds))
            .expect("a deadline inside the clock");
        let mut row = FlapRow {
            shape: shape.name(),
            to_relay: 0,
            relay_max_s: 0.0,
            relay_total_s: 0.0,
            rtt_max_ms: 0.0,
            rtt_last_ms: 0.0,
            closed: Vec::new(),
            stalls: 0,
            frames: 0,
            samples: 0,
        };
        let mut on_relay = false;
        let mut stretch = 0.0_f64;
        while tokio::time::Instant::now() < deadline {
            ticks.tick().await;
            row.samples = row.samples.saturating_add(1);
            if let Some(rtt) = link.rtt() {
                let ms = rtt.as_secs_f64() * 1e3;
                row.rtt_last_ms = ms;
                row.rtt_max_ms = row.rtt_max_ms.max(ms);
            }
            match link.relayed() {
                Some(true) => {
                    if !on_relay {
                        on_relay = true;
                        stretch = 0.0;
                        row.to_relay = row.to_relay.saturating_add(1);
                        eprintln!("flap {}: on a relay; paths: {}", shape.name(), link.paths());
                    }
                    stretch += step.as_secs_f64();
                    row.relay_total_s += step.as_secs_f64();
                    row.relay_max_s = row.relay_max_s.max(stretch);
                }
                Some(false) if on_relay => {
                    on_relay = false;
                    eprintln!("flap {}: back on a direct path after {stretch:.1} s", shape.name());
                }
                // Direct all along, or no path reported yet.
                Some(false) | None => {}
            }
        }
        drop(load);
        let stats = screen.stats();
        row.stalls = stats.stalls;
        row.frames = stats.frames;
        // What the transport said while the load ran: the closed-path lines carry noq's reason.
        let text = String::from_utf8_lossy(log.lock().get(mark..).unwrap_or_default()).into_owned();
        row.closed = text
            .lines()
            .filter(|line| line.contains("path closed") || line.contains("abandon"))
            .map(str::trim)
            .map(str::to_owned)
            .collect();
        eprintln!("flap {}: paths after load: {}", shape.name(), link.paths());
        drop(screen);
        link.send(ClientMsg::Screen(ScreenRequest::Close(stream))).await.unwrap();
        link.close();
        endpoint.close().await;
        Some(row)
    }

    /// A capture target that produces no frame at all: the host says so, and the receiver stops
    /// asking for refreshes no refresh can answer.
    ///
    /// The target is this test's own window ([`slopty-idle-window`]), on screen while the host
    /// lists it and ordered out before the stream opens, so ScreenCaptureKit has nothing to
    /// deliver. Gated on `SLOPTY_SCREEN_E2E`: it needs the screen-recording permission.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_window_that_never_draws_is_reported_idle_and_stops_the_asking() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let markers = dir.path().join("markers");
        std::fs::create_dir_all(&markers).unwrap();
        let title = format!("slopty idle {}", std::process::id());
        let mut helper = Command::new(bin_of("slopty-e2e", "slopty-idle-window"))
            .arg(&markers)
            .arg(&title)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn the idle window");
        let ready = markers.join("ready");
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_secs(20))
            .expect("a deadline inside the clock");
        while !ready.exists() {
            assert!(tokio::time::Instant::now() < deadline, "the idle window never opened");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let (_guard, ticket) = daemons(dir.path(), Reach::DirectOnly).await;
        let (endpoint, host) = dial(&ticket, Reach::DirectOnly).await;
        let mut link = slopty_client::HostLink::start(host);
        let mut events = link.events().unwrap();
        link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let target = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Listing { windows, .. })) => {
                    let found = windows.iter().find(|w| w.title == title);
                    break found.expect("the idle window in the listing").id;
                }
                _other => {}
            }
        };
        // Off screen before the stream opens: listed (the host enumerates with
        // `onScreenWindowsOnly: false`), captured, and never drawing.
        std::fs::write(markers.join("hide"), b"").unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;

        link.send(ClientMsg::Screen(ScreenRequest::Open {
            target: CaptureTarget::Window(target),
            quality: Quality::default(),
        }))
        .await
        .unwrap();
        let (stream, codec) = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Opened {
                    stream, codec, ..
                })) => break (stream, codec),
                LinkEvent::Control(HostMsg::Screen(ScreenEvent::Closed { reason, .. })) => {
                    panic!("open failed: {reason}")
                }
                _other => {}
            }
        };
        let screen = link.screen(stream, codec);

        // The host notices there is nothing to capture and says so, exactly as the app's canvas
        // would hear it.
        let mut states = Vec::new();
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("a deadline inside the clock");
        while tokio::time::Instant::now() < deadline {
            let Ok(Some(event)) =
                tokio::time::timeout(Duration::from_millis(500), events.recv()).await
            else {
                continue;
            };
            if let LinkEvent::Control(HostMsg::Screen(ScreenEvent::Source { state, .. })) = event {
                states.push(state);
                screen.set_source_live(state == SourceState::Live);
                if state == SourceState::Idle {
                    break;
                }
            }
        }
        assert_eq!(states.last(), Some(&SourceState::Idle), "the host never called it idle");

        // Told that, the receiver gives up asking: whatever it sent before the hint, it sends no
        // more of them over the next stretch, and the count is inside the cap either way.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let quiet = screen.stats();
        let cap = u64::from(slopty_media::Config::default().refresh_max_repeats);
        eprintln!("idle window: {} refreshes, cap {cap}", quiet.refreshes);
        assert!(
            quiet.refreshes <= cap,
            "{} refresh requests for a source that cannot answer one (cap {cap})",
            quiet.refreshes
        );
        assert_eq!(quiet.frames, 0, "an ordered-out window produced pictures");

        // Drawing again brings the stream back with no help from the client.
        std::fs::write(markers.join("show"), b"").unwrap();
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_secs(15))
            .expect("a deadline inside the clock");
        let mut live = false;
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(LinkEvent::Control(HostMsg::Screen(ScreenEvent::Source {
                state,
                ..
            })))) = tokio::time::timeout(Duration::from_millis(250), events.recv()).await
            {
                live |= state == SourceState::Live;
                screen.set_source_live(state == SourceState::Live);
            }
            if live && screen.stats().frames > 0 {
                break;
            }
        }
        let back = screen.stats();
        assert!(live, "the host never took the idle hint back");
        assert!(back.frames > 0, "no picture after the window drew again: {back:?}");

        std::fs::write(markers.join("quit"), b"").unwrap();
        let _stopped = helper.wait().await;
        drop(screen);
        link.close();
        endpoint.close().await;
    }

    /// [`bin`] for a binary whose package is not named after it.
    fn bin_of(package: &str, name: &str) -> PathBuf {
        let hostd = PathBuf::from(env!("CARGO_BIN_EXE_slopty-hostd"));
        let path = hostd.with_file_name(name);
        if !path.exists() {
            let release = hostd.parent().is_some_and(|dir| dir.ends_with("release"));
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let mut build = std::process::Command::new(cargo);
            build.args(["build", "-p", package, "--bin", name]);
            if release {
                build.arg("--release");
            }
            let status = build.status().expect("run cargo");
            assert!(status.success(), "build {name}");
        }
        path
    }
}
