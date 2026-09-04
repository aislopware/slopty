//! ptyd + hostd + a client, all on this machine: pair, open a shell, see its output, close it.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::Duration;

    use slopty_core::ClientId;
    use slopty_net::client::{HostConn, bind_client, connect_with_ticket};
    use slopty_net::pairing::PairTicket;
    use slopty_net::{ClientMsg, HostMsg, SecretKey};
    use slopty_proto::PROTOCOL_VERSION;
    use slopty_proto::handshake::{Caps, ClientKind, Hello};
    use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent, ScreenRequest};
    use slopty_proto::terminal::{OpenSession, TermEvent, TermRequest, TermSize};
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};

    const STEP: Duration = Duration::from_secs(20);

    /// A sibling binary from the same build. `cargo test -p slopty-hostd` on its own does not
    /// build ptyd, so build it on demand.
    fn bin(name: &str) -> PathBuf {
        let hostd = PathBuf::from(env!("CARGO_BIN_EXE_slopty-hostd"));
        let path = hostd.with_file_name(name);
        if !path.exists() {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let status = std::process::Command::new(cargo)
                .args(["build", "-p", name])
                .status()
                .expect("run cargo");
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
        let mut hostd = Command::new(bin("slopty-hostd"))
            .arg("--ptyd-socket")
            .arg(&ptyd_sock)
            .arg("--ctl-socket")
            .arg(&ctl_sock)
            .arg("--data-dir")
            .arg(dir.join("data"))
            .arg("--print-ticket")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdout = hostd.stdout.take().unwrap();
        let mut guard = Guard(vec![ptyd, hostd], None);
        let mut line = String::new();
        tokio::time::timeout(STEP, BufReader::new(stdout).read_line(&mut line))
            .await
            .expect("hostd prints a ticket")
            .unwrap();
        let ticket: PairTicket = line.trim().parse().unwrap();

        let endpoint = bind_client(SecretKey::generate()).await.unwrap();
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            client: ClientId::new(),
            kind: ClientKind::Tool,
            name: "e2e".to_owned(),
            app_version: "0".to_owned(),
            caps: Caps::empty(),
            pair_token: None,
        };
        let host = tokio::time::timeout(STEP, connect_with_ticket(&endpoint, &ticket, hello))
            .await
            .unwrap()
            .unwrap();
        guard.1 = Some(endpoint);
        (guard, host)
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

    /// Streams the first display through hostd and decodes it on the client side. Needs
    /// Screen Recording permission for the test process, so it only runs when
    /// `SLOPTY_SCREEN_E2E=1`.
    #[tokio::test]
    async fn screen_stream_over_iroh() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (_guard, mut host) = connect(dir.path()).await;

        host.tx.send(&ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let display = loop {
            match tokio::time::timeout(STEP, host.rx.recv()).await.unwrap().unwrap() {
                HostMsg::Screen(ScreenEvent::Listing { displays, windows }) => {
                    eprintln!("{} windows, {} displays", windows.len(), displays.len());
                    break displays.first().expect("a display").id;
                }
                HostMsg::Canvas(_sync) => {}
                other => panic!("unexpected control message before Listing: {other:?}"),
            }
        };

        let quality = Quality { fps: 60, bitrate_bps: 8_000_000, scale: 0.5, ..Quality::default() };
        let target = CaptureTarget::Display(display);
        host.tx.send(&ClientMsg::Screen(ScreenRequest::Open { target, quality })).await.unwrap();
        let (stream, codec, width, height) = loop {
            match tokio::time::timeout(STEP, host.rx.recv()).await.unwrap().unwrap() {
                HostMsg::Screen(ScreenEvent::Opened { stream, codec, width, height, .. }) => {
                    break (stream, codec, width, height);
                }
                HostMsg::Screen(ScreenEvent::Closed { reason, .. }) => {
                    panic!("open failed: {reason}")
                }
                HostMsg::Canvas(_sync) => {}
                other => panic!("unexpected control message before Opened: {other:?}"),
            }
        };
        eprintln!("opened {stream} {codec:?} {width}x{height}");

        let decoded = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let sink_count = std::sync::Arc::clone(&decoded);
        let mut decoder = slopty_codec::Decoder::new(codec, move |frame| {
            assert_eq!(
                (frame.image.width(), frame.image.height()),
                (width as usize, height as usize)
            );
            sink_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        let mut reassembler = slopty_media::Reassembler::new(
            stream,
            slopty_media::Config::default(),
            std::time::Instant::now(),
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut frames = 0_u32;
        let mut cursor = 0_u32;
        let mut first_keyframe = None;
        while tokio::time::Instant::now() < deadline {
            let datagram = match tokio::time::timeout_at(deadline, host.conn.read_datagram()).await
            {
                Ok(Ok(d)) => d,
                Ok(Err(e)) => panic!("read_datagram: {e}"),
                Err(_elapsed) => break,
            };
            let now = std::time::Instant::now();
            match reassembler.ingest(&datagram, now) {
                slopty_media::Ingest::Cursor { .. } => cursor += 1,
                slopty_media::Ingest::Video => {
                    while let Some(frame) = reassembler.next_frame() {
                        frames += 1;
                        first_keyframe.get_or_insert(frame.info.keyframe);
                        if let Some(token) = frame.info.ltr_token {
                            reassembler.ack_ltr(token);
                        }
                        decoder.decode(&frame.data, u64::from(frame.info.capture_ts_us)).unwrap();
                    }
                }
                _other => {}
            }
            for action in reassembler.tick(now, Duration::from_millis(2)) {
                let req = match action {
                    slopty_media::Action::Nack { frame, fragments } => {
                        ScreenRequest::Nack { stream, frame, fragments }
                    }
                    slopty_media::Action::RequestRefresh { last_good_frame } => {
                        ScreenRequest::RequestRefresh { stream, last_good_frame }
                    }
                };
                host.tx.send(&ClientMsg::Screen(req)).await.unwrap();
            }
        }
        let report = reassembler.take_report(0);
        host.tx.send(&ClientMsg::Screen(ScreenRequest::Report { stream, report })).await.unwrap();
        let stats = reassembler.stats();
        eprintln!(
            "frames {frames}, decoded {}, cursor {cursor}, {stats:?}",
            decoded.load(std::sync::atomic::Ordering::Relaxed)
        );
        assert_eq!(first_keyframe, Some(true), "stream starts with a keyframe");
        assert!(frames >= 10, "expected a steady stream, got {frames} frames");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(decoded.load(std::sync::atomic::Ordering::Relaxed) >= frames.saturating_sub(3));

        host.tx.send(&ClientMsg::Screen(ScreenRequest::Close(stream))).await.unwrap();
        loop {
            match tokio::time::timeout(STEP, host.rx.recv()).await.unwrap().unwrap() {
                HostMsg::Screen(ScreenEvent::Closed { stream: s, .. }) => {
                    assert_eq!(s, stream);
                    break;
                }
                _other => {}
            }
        }
    }
}
