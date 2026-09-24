//! ptyd + worker + a client, all on this machine: connect, open a shell, see its output, close it.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::Duration;

    use slopty_client::LinkEvent;
    use slopty_core::{ClientId, SessionId, WindowId};
    use slopty_net::client::{WorkerConn, bind_client, connect_addr};
    use slopty_net::framed::FramedRecv;
    use slopty_net::streams::{self, Uni};
    use slopty_net::{ClientMsg, WorkerMsg};
    use slopty_proto::PROTOCOL_VERSION;
    use slopty_proto::handshake::{Caps, ClientKind, Hello};
    use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent, ScreenRequest, SourceState};
    use slopty_proto::terminal::{OpenSession, TermEvent, TermRequest, TermSize};
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};

    const STEP: Duration = Duration::from_secs(20);

    /// A sibling binary from the same build. `cargo test -p slopty-workerd` on its own does not
    /// build ptyd, so build it on demand — into the profile directory this test binary came
    /// from, or a `--release` run would build a debug ptyd and then look for it beside the
    /// release the worker.
    fn bin(name: &str) -> PathBuf {
        let worker = PathBuf::from(env!("CARGO_BIN_EXE_slopty-worker"));
        let path = worker.with_file_name(name);
        if !path.exists() {
            let release = worker.parent().is_some_and(|dir| dir.ends_with("release"));
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

    /// Keeps the daemons and the client endpoint alive for the test.
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

    /// Start ptyd and the worker in `dir`, connect a client, return the daemons and the connection.
    async fn connect(dir: &std::path::Path) -> (Guard, WorkerConn) {
        let (mut guard, addr) = daemons(dir).await;
        let (endpoint, worker) = dial(addr).await;
        guard.1 = Some(endpoint);
        (guard, worker)
    }

    /// Wait for ptyd to accept connections on `socket`, checking for early exit.
    async fn wait_for_ptyd(child: &mut Child, socket: &std::path::Path) {
        let deadline =
            tokio::time::Instant::now().checked_add(Duration::from_secs(60)).expect("deadline");
        let mut ready = false;
        loop {
            if let Some(status) = child.try_wait().expect("poll ptyd") {
                panic!(
                    "ptyd exited early with status {status} while waiting for ptyd.sock (deadline 60 s)"
                );
            }
            if tokio::net::UnixStream::connect(socket).await.is_ok() {
                ready = true;
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            // Poll interval: attempts UnixStream connect to ptyd socket every 25 ms.
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if !ready {
            let _kill = child.start_kill();
            panic!("ptyd socket not ready after waiting 60 s");
        }
    }

    /// Start ptyd and the worker in `dir`; the address a loopback client dials.
    async fn daemons(dir: &std::path::Path) -> (Guard, SocketAddr) {
        let ptyd_sock = dir.join("ptyd.sock");
        let mut ptyd = Command::new(bin("slopty-ptyd"))
            .arg("--socket")
            .arg(&ptyd_sock)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("slopty-ptyd built alongside the tests");
        wait_for_ptyd(&mut ptyd, &ptyd_sock).await;
        let (worker, addr) = spawn_worker(dir).await;
        let guard = Guard(vec![ptyd, worker], None);
        (guard, addr)
    }

    /// Start the worker on `dir`'s ptyd socket and data dir and read the address it prints.
    async fn spawn_worker(dir: &std::path::Path) -> (Child, SocketAddr) {
        let ptyd_sock = dir.join("ptyd.sock");
        let ctl_sock = dir.join("worker.sock");
        let mut worker = Command::new(bin("slopty-worker"))
            .arg("--ptyd-socket")
            .arg(&ptyd_sock)
            .arg("--ctl-socket")
            .arg(&ctl_sock)
            .arg("--data-dir")
            .arg(dir.join("data"))
            .arg("--print-addr")
            // Any free port: the developer's own the worker may hold the default one.
            .arg("--port")
            .arg("0")
            // Never the user's clipboard, never their home.
            .env("SLOPTY_PASTEBOARD", pasteboard_name(dir))
            .env("SLOPTY_DROP_DIR", dir.join("drop"))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdout = worker.stdout.take().unwrap();
        let mut line = String::new();
        let read = tokio::time::timeout(STEP, BufReader::new(stdout).read_line(&mut line)).await;
        match read {
            Ok(Ok(_n)) => {}
            Ok(Err(err)) => {
                let status = worker.try_wait().ok().flatten();
                panic!(
                    "failed to read the worker's address after waiting {STEP:?}: worker exit status: {status:?}, io error: {err}"
                );
            }
            Err(_elapsed) => {
                let status = worker.try_wait().ok().flatten();
                panic!(
                    "the worker did not print its address after waiting {STEP:?}: worker exit status: {status:?}"
                );
            }
        }
        let bound: SocketAddr = match line.trim().parse() {
            Ok(addr) => addr,
            Err(err) => {
                let status = worker.try_wait().ok().flatten();
                panic!(
                    "the worker printed {line:?}, not an address; worker exit status: {status:?}, parse error: {err}"
                );
            }
        };
        (worker, dialable(bound))
    }

    /// Where a client on this machine reaches a worker bound at `bound`: loopback when it took
    /// every interface.
    fn dialable(bound: SocketAddr) -> SocketAddr {
        if bound.ip().is_unspecified() {
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, bound.port()))
        } else {
            bound
        }
    }

    /// A fresh endpoint (new client id) dialing `addr`: a cold QUIC connection.
    async fn dial(addr: SocketAddr) -> (slopty_net::Endpoint, WorkerConn) {
        let endpoint = bind_client().unwrap();
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            client: ClientId::new(),
            kind: ClientKind::Tool,
            name: "e2e".to_owned(),
            app_version: "0".to_owned(),
            caps: Caps::empty(),
        };
        let worker = tokio::time::timeout(STEP, connect_addr(&endpoint, addr, hello))
            .await
            .unwrap()
            .unwrap();
        (endpoint, worker)
    }

    /// The next session stream the worker opens: its session and its events.
    async fn session_stream(worker: &WorkerConn) -> (SessionId, FramedRecv<TermEvent>) {
        match tokio::time::timeout(STEP, streams::accept_uni(&worker.conn)).await.unwrap().unwrap()
        {
            Uni::Session { session, rx } => (session, rx),
            Uni::Bulk { header, .. } => panic!("a bulk stream, not a session: {header:?}"),
        }
    }

    /// Close a test's endpoint, giving its connection a moment to tell the worker.
    async fn close_endpoint(endpoint: &slopty_net::Endpoint) {
        endpoint.close(0_u32.into(), b"done");
        let _drained = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
    }

    /// What a shaper binds: any interface, any free port.
    fn wildcard() -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0], 0))
    }

    /// The address of a worker we did not start, read from its control socket (`doctor` reports
    /// where it listens).
    async fn listening(ctl_sock: &std::path::Path) -> SocketAddr {
        use tokio::io::AsyncWriteExt as _;
        let stream = tokio::net::UnixStream::connect(ctl_sock).await.unwrap();
        let (rd, mut wr) = stream.into_split();
        let mut line = serde_json::to_vec(&slopty_worker::ctl::CtlRequest::Doctor).unwrap();
        line.push(b'\n');
        wr.write_all(&line).await.unwrap();
        // The worker answers a control request and closes without waiting to be half-closed, so
        // this shutdown races its close and macOS reports ENOTCONN when it loses. The reply
        // is already buffered by then; let `read_line` below be the judge of whether one
        // arrived.
        if let Err(e) = wr.shutdown().await {
            assert_eq!(e.kind(), std::io::ErrorKind::NotConnected, "control socket shutdown: {e}");
        }
        let mut reply = String::new();
        BufReader::new(rd).read_line(&mut reply).await.unwrap();
        match serde_json::from_str(reply.trim()).unwrap() {
            slopty_worker::ctl::CtlReply::Doctor(health) => {
                dialable(health.listen.parse().unwrap())
            }
            other => panic!("unexpected ctl reply: {other:?}"),
        }
    }

    #[tokio::test]
    async fn shell_round_trip_over_quic() {
        let dir = tempfile::tempdir().unwrap();
        let (_guard, mut worker) = connect(dir.path()).await;
        assert_eq!(worker.ack.protocol, PROTOCOL_VERSION);
        assert!(worker.ack.sessions.is_empty());

        let size = TermSize { cols: 40, rows: 6, ..TermSize::default() };
        // `~` as the directory: the client does not know the worker's home.
        let home = std::env::var("HOME").unwrap();
        worker
            .tx
            .send(&ClientMsg::OpenSession(OpenSession {
                size,
                cwd: Some("~".to_owned()),
                command: vec!["/bin/sh".to_owned()],
                env: vec![("PS1".to_owned(), "$ ".to_owned())],
                title: None,
                attach: true,
            }))
            .await
            .unwrap();
        // The item snapshot (sent right after HelloAck) and the item delta for the new
        // terminal item interleave with SessionOpened on the control stream; skip them.
        let session = loop {
            match tokio::time::timeout(STEP, worker.rx.recv()).await.unwrap().unwrap() {
                WorkerMsg::SessionOpened(summary) => break summary.id,
                WorkerMsg::Items(_sync) => {}
                other => panic!("unexpected control message before SessionOpened: {other:?}"),
            }
        };
        let (opened, mut events) = session_stream(&worker).await;
        assert_eq!(opened, session);

        worker
            .tx
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
        worker
            .tx
            .send(&ClientMsg::Term {
                session,
                req: TermRequest::Raw(b"echo cwd=$(pwd)\n".to_vec()),
            })
            .await
            .unwrap();
        wait_for_text(&mut events, &format!("cwd={home}")).await;

        worker.tx.send(&ClientMsg::Term { session, req: TermRequest::Close }).await.unwrap();
        loop {
            match tokio::time::timeout(STEP, worker.rx.recv()).await.unwrap().unwrap() {
                WorkerMsg::SessionClosed { session: s, .. } => {
                    assert_eq!(s, session);
                    break;
                }
                _other => {}
            }
        }
        worker.tx.send(&ClientMsg::Ping { sent_at: slopty_core::MonoTime::now() }).await.unwrap();
        loop {
            match tokio::time::timeout(STEP, worker.rx.recv()).await.unwrap().unwrap() {
                WorkerMsg::Pong { .. } => break,
                WorkerMsg::Items(_sync) => {}
                other => panic!("unexpected control message before Pong: {other:?}"),
            }
        }
    }

    /// Keystroke echo while the same connection keeps the worker busy with slow requests: quick
    /// open walking 20 000 files, and window streams opening (onto the idle window where Screen
    /// Recording is granted; failing at ScreenCaptureKit where it is not). None of that may put
    /// a terminal's echo behind it.
    #[tokio::test(flavor = "multi_thread")]
    async fn echo_is_not_held_behind_slow_requests() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        for d in 0..200 {
            let sub = tree.join(format!("d{d:03}"));
            std::fs::create_dir_all(&sub).unwrap();
            for f in 0..100 {
                std::fs::write(sub.join(format!("f{f:03}.txt")), b"").unwrap();
            }
        }
        let markers = dir.path().join("markers");
        std::fs::create_dir_all(&markers).unwrap();
        let title = format!("slopty echo {}", std::process::id());
        let mut helper = Command::new(bin_of("slopty-e2e", "slopty-idle-window"))
            .arg(&markers)
            .arg(&title)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn the idle window");
        wait_for_marker(&markers.join("ready"), "the idle window").await;

        let (_guard, mut worker) = connect(dir.path()).await;
        let size = TermSize { cols: 80, rows: 24, ..TermSize::default() };
        worker
            .tx
            .send(&ClientMsg::OpenSession(OpenSession {
                size,
                cwd: None,
                command: vec!["/bin/cat".to_owned()],
                env: Vec::new(),
                title: None,
                attach: true,
            }))
            .await
            .unwrap();
        let (_session, mut frames) = session_stream(&worker).await;
        let session = next_msg(&mut worker, |m| match m {
            WorkerMsg::SessionOpened(summary) => Some(summary.id),
            _ => None,
        })
        .await;

        // Where Screen Recording is granted the listing names the idle window; where it is not
        // there is no listing, and an open fails at ScreenCaptureKit instead.
        worker.tx.send(&ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let window = tokio::time::timeout(
            Duration::from_secs(2),
            next_msg(&mut worker, |m| match m {
                WorkerMsg::Screen(ScreenEvent::Listing { windows, .. }) => {
                    Some(windows.iter().find(|w| w.title == title).map(|w| w.id))
                }
                _ => None,
            }),
        )
        .await
        .ok()
        .flatten();
        eprintln!("idle window: {window:?}");
        let target = CaptureTarget::Window(window.unwrap_or(WindowId(u32::MAX)));

        let WorkerConn { tx, rx, .. } = worker;
        let (send, mut outbox) = tokio::sync::mpsc::channel::<ClientMsg>(64);
        let writer = tokio::spawn(async move {
            let mut tx = tx;
            while let Some(msg) = outbox.recv().await {
                tx.send(&msg).await.unwrap();
            }
        });
        // Every answer to a slow request, and the streams to close again.
        let (answered, mut answers) = tokio::sync::mpsc::channel::<WorkerMsg>(64);
        let reader = tokio::spawn(async move {
            let mut rx = rx;
            while let Ok(msg) = rx.recv().await {
                if matches!(msg, WorkerMsg::FoundFiles { .. } | WorkerMsg::Screen(_))
                    && answered.send(msg).await.is_err()
                {
                    break;
                }
            }
        });

        let baseline = echoes(&send, &mut frames, session, 60).await;
        let busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let load = {
            let (send, busy) = (send.clone(), std::sync::Arc::clone(&busy));
            let root = tree.to_string_lossy().into_owned();
            tokio::spawn(async move {
                let mut walks = Vec::new();
                let mut opens = Vec::new();
                while busy.load(std::sync::atomic::Ordering::Relaxed) {
                    let started = std::time::Instant::now();
                    let find = ClientMsg::FindFiles { root: root.clone(), query: "nothing".into() };
                    send.send(find).await.unwrap();
                    let open = ScreenRequest::Open { target, quality: Quality::default() };
                    send.send(ClientMsg::Screen(open)).await.unwrap();
                    let (mut found, mut screen) = (false, false);
                    while !(found && screen) {
                        match tokio::time::timeout(STEP, answers.recv()).await.unwrap().unwrap() {
                            WorkerMsg::FoundFiles { .. } => {
                                found = true;
                                walks.push(started.elapsed().as_secs_f64() * 1e3);
                            }
                            WorkerMsg::Screen(ScreenEvent::Opened { stream, .. }) => {
                                screen = true;
                                opens.push(started.elapsed().as_secs_f64() * 1e3);
                                let close = ClientMsg::Screen(ScreenRequest::Close(stream));
                                send.send(close).await.unwrap();
                            }
                            WorkerMsg::Screen(ScreenEvent::Closed { .. }) if !screen => {
                                screen = true;
                                opens.push(started.elapsed().as_secs_f64() * 1e3);
                            }
                            _other => {}
                        }
                    }
                }
                (walks, opens)
            })
        };
        let loaded = echoes(&send, &mut frames, session, 200).await;
        busy.store(false, std::sync::atomic::Ordering::Relaxed);
        let (walks, opens) = load.await.unwrap();

        let (b50, b99, bmax) = p50_p99_max(&baseline);
        let (l50, l99, lmax) = p50_p99_max(&loaded);
        let (w50, _w99, wmax) = p50_p99_max(&walks);
        let (o50, _o99, omax) = p50_p99_max(&opens);
        eprintln!(
            "echo idle p50 {b50:.2} / p99 {b99:.2} / max {bmax:.2} ms; under load p50 {l50:.2} / \
             p99 {l99:.2} / max {lmax:.2} ms; quick open p50 {w50:.1} / max {wmax:.1} ms over {}; \
             window open p50 {o50:.1} / max {omax:.1} ms over {}",
            walks.len(),
            opens.len()
        );
        // The median, not the tail: echo queued behind the handlers sat near a quick open's own
        // time (p50 28.7 ms), while a parallel test run alone stretches the tail past 20 ms.
        assert!(
            l50 < w50 / 4.0,
            "echo p50 {l50:.2} ms waits behind a {w50:.1} ms quick open on the same connection"
        );

        writer.abort();
        reader.abort();
        std::fs::write(markers.join("quit"), b"").unwrap();
        let _stopped = helper.wait().await;
    }

    fn p50_p99_max(samples: &[f64]) -> (f64, f64, f64) {
        let mut sorted = samples.to_vec();
        let (p50, _p90, max) = quantiles(&mut sorted);
        let last = sorted.len().saturating_sub(1);
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "an index below 2^53"
        )]
        let i = ((last as f64) * 0.99).round() as usize;
        (p50, sorted.get(i).copied().unwrap_or(0.0), max)
    }

    /// Type `count` keys into a `/bin/cat` session one at a time, each after the last one's echo
    /// came back; the round trips in milliseconds.
    async fn echoes(
        send: &tokio::sync::mpsc::Sender<ClientMsg>,
        frames: &mut FramedRecv<TermEvent>,
        session: SessionId,
        count: usize,
    ) -> Vec<f64> {
        let mut took = Vec::with_capacity(count);
        for i in 0..count {
            let key = b'a'.saturating_add(u8::try_from(i % 26).unwrap());
            let sent = std::time::Instant::now();
            let req = TermRequest::Raw(vec![key]);
            send.send(ClientMsg::Term { session, req }).await.unwrap();
            loop {
                let ev = tokio::time::timeout(STEP, frames.recv()).await.unwrap().unwrap();
                if matches!(ev, TermEvent::Frame(_)) {
                    break;
                }
            }
            took.push(sent.elapsed().as_secs_f64() * 1e3);
            // A line of 64: the terminal's line buffer stays far from full.
            if i % 64 == 63 {
                send.send(ClientMsg::Term { session, req: TermRequest::Raw(b"\x15".to_vec()) })
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(30)).await;
                while tokio::time::timeout(Duration::from_millis(30), frames.recv()).await.is_ok() {
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        took
    }

    /// Open a shell, have it print `marker`, and wait for the marker on the screen.
    async fn open_shell_and_see(
        worker: &mut WorkerConn,
        marker: &str,
    ) -> (SessionId, FramedRecv<TermEvent>) {
        let size = TermSize { cols: 40, rows: 6, ..TermSize::default() };
        worker
            .tx
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
        let session = loop {
            match tokio::time::timeout(STEP, worker.rx.recv()).await.unwrap().unwrap() {
                WorkerMsg::SessionOpened(summary) => break summary.id,
                WorkerMsg::Items(_sync) => {}
                other => panic!("unexpected control message before SessionOpened: {other:?}"),
            }
        };
        let (opened, mut events) = session_stream(worker).await;
        assert_eq!(opened, session);
        // Quoted so the echoed command line never matches, only the output.
        let (head, tail) = marker.split_at(marker.len() / 2);
        worker
            .tx
            .send(&ClientMsg::Term {
                session,
                req: TermRequest::Raw(format!("echo {head}'{tail}'\n").into_bytes()),
            })
            .await
            .unwrap();
        wait_for_text(&mut events, marker).await;
        (session, events)
    }

    /// Wait until a frame carries a row containing `text`.
    async fn wait_for_text(events: &mut FramedRecv<TermEvent>, text: &str) {
        let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
        loop {
            let ev = tokio::time::timeout_at(deadline, events.recv()).await.unwrap().unwrap();
            if let TermEvent::Frame(f) = ev
                && f.updates.iter().any(|u| u.line.text().contains(text))
            {
                break;
            }
        }
    }

    /// The terminal lives in ptyd; the screen lives in the worker's engine. When the worker dies
    /// and comes Two clients on one worker: one pointing at an item reaches the other in its
    /// name, as is (the worker keeps nothing and checks nothing: an item the other lacks is its
    /// to ignore).
    #[tokio::test]
    async fn a_pointing_reaches_the_others() {
        use slopty_proto::items::ItemSync;

        let dir = tempfile::tempdir().unwrap();
        let (mut guard, addr) = daemons(dir.path()).await;
        let (endpoint_a, mut a) = dial(addr).await;
        guard.1 = Some(endpoint_a);
        let (endpoint_b, mut b) = dial(addr).await;
        let item = slopty_core::ItemId::new();
        a.tx.send(&ClientMsg::Point { item }).await.unwrap();
        let mine = next_items(&mut a, |s| matches!(s, ItemSync::Pointed { .. })).await;
        let ItemSync::Pointed { client: a_client, .. } = mine else { panic!("{mine:?}") };
        let heard = next_items(&mut b, |s| matches!(s, ItemSync::Pointed { .. })).await;
        assert_eq!(heard, ItemSync::Pointed { client: a_client, name: "e2e".to_owned(), item });
        drop(endpoint_b);
    }

    /// A file behind a file card is watched: a write on the worker reaches the client as a
    /// fresh `WorkerMsg::File` unasked, its removal too, and an emptied watch list stops it.
    #[tokio::test]
    async fn a_watched_file_is_read_again_when_it_changes() {
        use slopty_proto::file::FileRead;

        let dir = tempfile::tempdir().unwrap();
        let (_guard, mut worker) = connect(dir.path()).await;
        let path = dir.path().join("watched.txt");
        std::fs::write(&path, "one").unwrap();
        let name = path.to_string_lossy().into_owned();
        worker.tx.send(&ClientMsg::ReadFile { path: name.clone() }).await.unwrap();
        let read = next_file(&mut worker, &name).await;
        assert!(matches!(&read, FileRead::Text { text, .. } if text == "one"), "{read:?}");

        worker.tx.send(&ClientMsg::WatchFiles { paths: vec![name.clone()] }).await.unwrap();
        // The watch stamps the file as it is; the write must come after that.
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(&path, "two").unwrap();
        let read = next_file(&mut worker, &name).await;
        assert!(matches!(&read, FileRead::Text { text, .. } if text == "two"), "{read:?}");
        std::fs::remove_file(&path).unwrap();
        let read = next_file(&mut worker, &name).await;
        assert!(matches!(read, FileRead::Missing { .. }), "{read:?}");

        worker.tx.send(&ClientMsg::WatchFiles { paths: Vec::new() }).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(&path, "three").unwrap();
        let quiet =
            tokio::time::timeout(Duration::from_millis(2500), next_file(&mut worker, &name));
        assert!(quiet.await.is_err(), "nothing is read once the watch is dropped");
    }

    /// The next `WorkerMsg::File` for `path` on `worker`'s control stream, skipping everything
    /// else.
    async fn next_file(worker: &mut WorkerConn, path: &str) -> slopty_proto::file::FileRead {
        loop {
            match tokio::time::timeout(STEP, worker.rx.recv()).await.unwrap().unwrap() {
                WorkerMsg::File { path: p, read } if p == path => break read,
                _other => {}
            }
        }
    }

    /// The next item sync `wanted` on `worker`'s control stream, skipping everything else.
    async fn next_items(
        worker: &mut WorkerConn,
        wanted: impl Fn(&slopty_proto::items::ItemSync) -> bool,
    ) -> slopty_proto::items::ItemSync {
        loop {
            match tokio::time::timeout(STEP, worker.rx.recv()).await.unwrap().unwrap() {
                WorkerMsg::Items(sync) if wanted(&sync) => break sync,
                _other => {}
            }
        }
    }

    /// A worker that comes back on the same ptyd shows the screen the shell drew before, rebuilt
    /// from the checkpoint and the tapped output ptyd kept, not a blank grid.
    #[tokio::test]
    async fn a_worker_restart_keeps_the_screen() {
        let dir = tempfile::tempdir().unwrap();
        let (mut guard, mut worker) = connect(dir.path()).await;
        let (session, events) = open_shell_and_see(&mut worker, "restart-marker-one").await;
        // Past the checkpoint delay (500 ms after the last output): the state, not the ring,
        // is what the next worker replays.
        tokio::time::sleep(Duration::from_millis(900)).await;
        let mut old = guard.0.pop().unwrap();
        old.start_kill().unwrap();
        old.wait().await.unwrap();
        drop(events);
        drop(worker);

        let (worker, addr) = spawn_worker(dir.path()).await;
        guard.0.push(worker);
        let (endpoint, mut worker) = dial(addr).await;
        guard.1 = Some(endpoint);
        assert_eq!(worker.ack.sessions.iter().map(|s| s.id).collect::<Vec<_>>(), [session]);
        let size = TermSize { cols: 40, rows: 6, ..TermSize::default() };
        worker
            .tx
            .send(&ClientMsg::Term { session, req: TermRequest::Attach { size } })
            .await
            .unwrap();
        let (opened, mut events) = session_stream(&worker).await;
        assert_eq!(opened, session);
        let full = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                TermEvent::Frame(f) if f.full => break f,
                _other => {}
            }
        };
        let rows: Vec<String> =
            full.updates.iter().map(|u| u.line.text().trim_end().to_owned()).collect();
        assert!(
            rows.iter().any(|r| r == "restart-marker-one"),
            "the screen after the restart lost the marker: {rows:?}"
        );
        // The shell behind it is the same one, still taking input.
        worker
            .tx
            .send(&ClientMsg::Term {
                session,
                req: TermRequest::Raw(b"echo restart-mark'er-two'\n".to_vec()),
            })
            .await
            .unwrap();
        wait_for_text(&mut events, "restart-marker-two").await;
        worker.tx.send(&ClientMsg::Term { session, req: TermRequest::Close }).await.unwrap();
    }

    /// Streams the first display through the worker into the real client stack (`WorkerLink` +
    /// `ScreenHandle`): reassembly, NACK/report traffic and hardware decode all run as the app
    /// would run them. Needs Screen Recording permission for the test process, so it only runs
    /// when `SLOPTY_SCREEN_E2E=1`.
    #[tokio::test]
    async fn screen_stream_over_quic() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (_guard, worker) = connect(dir.path()).await;
        let mut link = slopty_client::WorkerLink::start(worker);
        let mut events = link.events().unwrap();

        link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let display = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Listing {
                    displays,
                    windows,
                })) => {
                    eprintln!("{} windows, {} displays", windows.len(), displays.len());
                    break displays.first().expect("a display").id;
                }
                LinkEvent::Control(WorkerMsg::Items(_sync)) => {}
                other => panic!("unexpected event before Listing: {other:?}"),
            }
        };

        let quality = Quality { fps: 60, bitrate_bps: 8_000_000, scale: 0.5, ..Quality::default() };
        let target = CaptureTarget::Display(display);
        link.send(ClientMsg::Screen(ScreenRequest::Open { target, quality })).await.unwrap();
        let (stream, codec, width, height) = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Opened {
                    stream,
                    codec,
                    width,
                    height,
                    ..
                })) => break (stream, codec, width, height),
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Closed { reason, .. })) => {
                    panic!("open failed: {reason}")
                }
                LinkEvent::Control(WorkerMsg::Items(_sync)) => {}
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
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Closed {
                    stream: s, ..
                })) => {
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
        /// The worker's bitrate decisions: `target(verdict)`.
        rate: String,
        /// What the presentation path did with the stream: the same `Pacer` the GPUI element
        /// runs, fed the same `frames` channel and paced by a 60 Hz timer standing in for the
        /// display link.
        pacing: slopty_client::PacingStats,
        /// Where the connection was sending when the sample ended: the shaper's address on a
        /// shaped run.
        selected: Option<SocketAddr>,
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
    ///
    /// `addr` is the worker's own address, or a shaper's in front of it.
    async fn start_up_sample(addr: SocketAddr, seconds: u64) -> StartUp {
        let (endpoint, worker) = dial(addr).await;
        let conn = worker.conn.clone();
        let mut link = slopty_client::WorkerLink::start(worker);
        let mut events = link.events().unwrap();
        link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let display = loop {
            // A refused listing has no reply to catch — the worker logs it and answers nothing — so
            // a missing TCC grant arrives here as a timeout, which says nothing on its
            // own.
            let Ok(event) = tokio::time::timeout(STEP, events.recv()).await else {
                panic!(
                    "no screen listing after {STEP:?}. The worker logs -3801 when it is refused Screen \
                     Recording, which is every worker but the installed one: TCC attributes a \
                     shell-spawned daemon to whatever launched it, so signing does not help. Run \
                     against `slopty worker install`'s worker."
                );
            };
            match event.unwrap() {
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Listing {
                    displays, ..
                })) => {
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
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Opened {
                    stream,
                    codec,
                    ..
                })) => {
                    break (stream, codec);
                }
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Closed { reason, .. })) => {
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
                // Measurement window: samples presentation and frames over the requested duration; no event to wait for.
                () = tokio::time::sleep_until(deadline) => break,
                ev = events.recv() => match ev {
                    Some(LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Rate { target_bps, verdict, capped, .. }))) => {
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
        // Read before the connection closes: a closed connection has no path.
        sample.selected = slopty_net::endpoint::remote(&conn);
        assert_eq!(stats.decode_errors, 0, "{stats:?}");
        drop(screen);
        link.send(ClientMsg::Screen(ScreenRequest::Close(stream))).await.unwrap();
        loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Closed {
                    stream: s, ..
                })) if s == stream => {
                    break;
                }
                _other => {}
            }
        }
        link.close();
        close_endpoint(&endpoint).await;
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
    async fn loss_sample(addr: SocketAddr, seconds: u64, drop_permille: u32) -> LossRow {
        let (endpoint, worker) = dial(addr).await;
        let mut link = slopty_client::WorkerLink::start(worker);
        // Deterministic: the same rate always drops the same datagrams of the sequence.
        link.screens().set_loss(drop_permille);
        let mut events = link.events().unwrap();
        link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let display = loop {
            // A refused listing has no reply to catch — the worker logs it and answers nothing — so
            // a missing TCC grant arrives here as a timeout, which says nothing on its
            // own.
            let Ok(event) = tokio::time::timeout(STEP, events.recv()).await else {
                panic!(
                    "no screen listing after {STEP:?}. The worker logs -3801 when it is refused Screen \
                     Recording, which is every worker but the installed one: TCC attributes a \
                     shell-spawned daemon to whatever launched it, so signing does not help. Run \
                     against `slopty worker install`'s worker."
                );
            };
            match event.unwrap() {
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Listing {
                    displays, ..
                })) => {
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
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Opened {
                    stream,
                    codec,
                    ..
                })) => break (stream, codec),
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Closed { reason, .. })) => {
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
                // Measurement window: streams under injected packet loss for the requested duration; no event to wait for.
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
        close_endpoint(&endpoint).await;
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
        let (_guard, addr) = daemons(dir.path()).await;
        // Waits for the worker's background ScreenCaptureKit warm-up to finish; the worker exposes
        // no observable state for it.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut rows = Vec::new();
        // 0/20/50 ‰ are the rates the ruling is about; 100 ‰ is the stress row that shows
        // what happens once parity alone cannot cover the loss.
        for permille in [0_u32, 20, 50, 100] {
            rows.push(loss_sample(addr, seconds, permille).await);
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
        // the datagram-level signal: worker and client share this process, so nothing but the
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
    /// The worker, native scale at the default quality (what the app opens). Prints a table; the
    /// numbers go to `docs/MEASUREMENTS.md`.
    #[tokio::test(flavor = "multi_thread")]
    async fn screen_start_up_over_quic() {
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
        // The app warms the decoder up at launch, long before it dials a worker.
        slopty_client::warm_up_decoder();
        let (_guard, addr) = daemons(dir.path()).await;
        // The daemon warms ScreenCaptureKit up right after it is online; by the time a user
        // opens a window that has long finished, so let it finish here too.
        // Waits for the worker's background ScreenCaptureKit warm-up to finish; the worker exposes
        // no observable state for it.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut rows = Vec::new();
        for i in 0..samples {
            let s = start_up_sample(addr, seconds).await;
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
            "| sample | opened | first datagram | first frame | first decoded | hold max | decoded | gap p50 / p90 / max | stalls (stalled) | nack / refresh / lost | worker target |"
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

    /// A shell through the shaper, with the client still on the shaper at the end of it.
    ///
    /// The ladder below is only worth reading while the shaper is the path. Plain QUIC dials one
    /// address and never looks for another, so this checks that it stays that way: a delayed
    /// link, a real session carried over it, and the address read after the traffic.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_shaped_link_keeps_the_client_on_the_shaper() {
        let dir = tempfile::tempdir().unwrap();
        let (_guard, worker_addr) = daemons(dir.path()).await;
        let link =
            slopty_shape::Link { delay: Duration::from_millis(30), ..slopty_shape::Link::CLEAR };
        let relay = std::sync::Arc::new(
            slopty_shape::relay::Relay::bind(wildcard(), worker_addr, link, 1).await.unwrap(),
        );
        let addr = relay.addr().unwrap();
        tokio::spawn({
            let relay = std::sync::Arc::clone(&relay);
            async move { relay.run().await }
        });

        let (_endpoint, mut worker) = dial(addr).await;
        worker
            .tx
            .send(&ClientMsg::OpenSession(OpenSession {
                size: TermSize { cols: 40, rows: 6, ..TermSize::default() },
                cwd: Some("~".to_owned()),
                command: vec!["/bin/sh".to_owned()],
                env: vec![("PS1".to_owned(), "$ ".to_owned())],
                title: None,
                attach: true,
            }))
            .await
            .unwrap();
        let session = loop {
            match tokio::time::timeout(STEP, worker.rx.recv()).await.unwrap().unwrap() {
                WorkerMsg::SessionOpened(summary) => break summary.id,
                WorkerMsg::Items(_sync) => {}
                other => panic!("unexpected control message before SessionOpened: {other:?}"),
            }
        };
        let (_opened, mut events) = session_stream(&worker).await;
        worker
            .tx
            .send(&ClientMsg::Term {
                session,
                req: TermRequest::Raw(b"echo marker-$((40+2))\n".to_vec()),
            })
            .await
            .unwrap();
        wait_for_text(&mut events, "marker-42").await;

        assert_eq!(
            slopty_net::endpoint::remote(&worker.conn),
            Some(addr),
            "the connection left the shaper; a shaped measurement would read an unshaped link. \
             path: {}",
            slopty_net::endpoint::describe_path(&worker.conn)
        );
        let carried = relay.carried().await;
        assert!(
            carried.up.sent > 0 && carried.down.sent > 0,
            "the shaper carried nothing in one direction: {carried:?}"
        );
        assert_eq!((carried.up.lost, carried.down.lost), (0, 0), "a clear link lost nothing");
    }

    /// One rung of the shaped ladder: the link, what got through, and what the shaper did.
    #[derive(Debug)]
    struct Rung {
        name: &'static str,
        /// `None` on the direct rung, which has no shaper between the ends.
        link: Option<slopty_shape::Link>,
        /// Where the shaper listened: the one address the client was given. `None` when direct.
        relay: Option<SocketAddr>,
        sample: StartUp,
        carried: slopty_shape::relay::Carried,
    }

    /// The links the ladder is measured on, easiest first.
    ///
    /// Two controls before any impairment. `direct` has no relay at all, so a row that reads
    /// wrong there is the harness or the worker rather than anything on this list. `clear` adds the
    /// relay process and its extra hop and shapes nothing, which separates the cost of being
    /// relayed from the cost of the link. The other three are shaped after the paths the
    /// congestion rulings argue about — a good Wi-Fi, an LTE hop, and the collapsed link where
    /// BBR3 starves and Cubic overshoots.
    const LADDER: &[(&str, Option<slopty_shape::Link>)] = &[
        ("direct", None),
        ("clear", Some(slopty_shape::Link::CLEAR)),
        (
            "wifi",
            Some(slopty_shape::Link {
                delay: Duration::from_millis(15),
                jitter: Duration::from_millis(5),
                loss: 0.002,
                rate: 4_000_000,
                queue: 1_000_000,
            }),
        ),
        (
            "lte",
            Some(slopty_shape::Link {
                delay: Duration::from_millis(60),
                jitter: Duration::from_millis(20),
                loss: 0.01,
                rate: 1_500_000,
                queue: 375_000,
            }),
        ),
        (
            "collapsed",
            Some(slopty_shape::Link {
                delay: Duration::from_millis(80),
                jitter: Duration::from_millis(30),
                loss: 0.03,
                rate: 600_000,
                queue: 150_000,
            }),
        ),
    ];

    /// Stream a display over each rung of [`LADDER`] and print what arrived.
    ///
    /// The impairment lives in a relay both ends speak QUIC through, so the congestion controller
    /// reacts to it exactly as it would to a bottleneck; `SLOPTY_E2E_SECONDS` (default 8) per
    /// rung. This is the harness the ⏸ keyframe-admission rule, the 🔬 cadence ladder and the ⏸
    /// audio jitter estimator were waiting on; the numbers go to `docs/MEASUREMENTS.md`.
    ///
    /// It runs against the *installed* worker rather than daemons of its own, which is not a
    /// preference: TCC attributes a shell-spawned daemon to whatever launched it, so it is refused
    /// capture with -3801 however the binary is signed, and only the launchd worker records a
    /// grant. `SLOPTY_E2E_WORKER_SOCKET` points at its control socket, under `<data dir>/run/`;
    /// the worker's address is read from it.
    #[tokio::test(flavor = "multi_thread")]
    async fn screen_over_a_shaped_link() {
        let Some(ctl_sock) = std::env::var_os("SLOPTY_E2E_WORKER_SOCKET") else {
            eprintln!(
                "SLOPTY_E2E_WORKER_SOCKET unset; skipping. Install the worker under launchd with \
                 `slopty worker install` and point this at its worker.sock; a spawned worker cannot \
                 get Screen Recording."
            );
            return;
        };
        let worker_addr = listening(&PathBuf::from(ctl_sock)).await;
        let seconds: u64 =
            std::env::var("SLOPTY_E2E_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
        let _logs = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .try_init();
        // Warmed up *before* the first rung, not alongside it. VideoToolbox serialises session
        // creation process-wide, so a warm-up still running when a stream starts holds that
        // stream's own session behind it — 35.8 s on the run that found this, which read as every
        // rung decoding nothing. `warm_up_decoder` is the app's fire-and-forget call; a
        // measurement has to wait for the answer.
        let warm = tokio::task::spawn_blocking(slopty_codec::warm_up).await.unwrap().unwrap();
        eprintln!("decoder warmed up in {warm:?}");
        // One sample thrown away before the table. Whichever stream is first in the process
        // decodes nothing however it is carried — proven by running the relay-less rung first
        // and then second: it read 0 frames, then 184. A warmed decoder is not enough on its
        // own, so the first rung would otherwise always be a blank row.
        let discarded = start_up_sample(worker_addr, 2).await;
        eprintln!("discarded the first stream: decoded {}", discarded.decoded);

        let mut rungs = Vec::new();
        for &(name, link) in LADDER {
            let Some(link) = link else {
                let sample = start_up_sample(worker_addr, seconds).await;
                eprintln!("{name}: selected {:?}, no shaper", sample.selected);
                rungs.push(Rung {
                    name,
                    link: None,
                    relay: None,
                    sample,
                    carried: slopty_shape::relay::Carried::default(),
                });
                continue;
            };
            // A shaper per rung: each starts with an empty queue and its own draws, so a rung
            // never inherits the standing queue the one before it left behind.
            let relay = std::sync::Arc::new(
                slopty_shape::relay::Relay::bind(wildcard(), worker_addr, link, 1).await.unwrap(),
            );
            let addr = relay.addr().unwrap();
            let carrying = tokio::spawn({
                let relay = std::sync::Arc::clone(&relay);
                async move { relay.run().await }
            });
            let sample = start_up_sample(addr, seconds).await;
            let carried = relay.carried().await;
            carrying.abort();
            eprintln!("{name}: selected {:?}, shaper {carried:?}", sample.selected);
            rungs.push(Rung { name, link: Some(link), relay: Some(addr), sample, carried });
        }

        eprintln!(
            "| link | rate | loss | first decoded | hold max | decoded | gap p50 / p90 / max | stalls (stalled) | nack / refresh / lost | shaper down: sent / lost / overflowed | worker target |"
        );
        for r in &rungs {
            let s = &r.sample;
            let d = r.carried.down;
            eprintln!(
                "| {} | {} kB/s | {:.1} % | {:.0} ms | {:.0} ms | {} | {:.1} / {:.1} / {:.1} ms | {} ({} ms) | {} / {} / {} | {} / {} / {} | {} |",
                r.name,
                r.link.map_or(0, |l| l.rate / 1_000),
                r.link.map_or(0.0, |l| f64::from(l.loss) * 100.0),
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
                d.sent,
                d.lost,
                d.overflowed,
                s.rate
            );
        }

        for r in &rungs {
            assert!(r.sample.decoded >= 1, "{}: decoded nothing: {r:?}", r.name);
            let Some(relay) = r.relay else { continue };
            // A relayed rung is only readable while the relay is the path.
            assert_eq!(
                r.sample.selected,
                Some(relay),
                "{}: the connection left the shaper, so this row is of an unshaped link",
                r.name
            );
            assert!(
                r.carried.up.sent > 0 && r.carried.down.sent > 0,
                "{}: nothing carried",
                r.name
            );
        }
        let clear = rungs.iter().find(|r| r.name == "clear").expect("the clear rung");
        assert_eq!((clear.carried.up.lost, clear.carried.down.lost), (0, 0), "{clear:?}");
        assert_eq!(clear.carried.down.overflowed, 0, "a clear link has no queue to overflow");
        // Every shaped rung must actually have degraded something, or its row is the clear row
        // with a different name on it. Both directions together: at the mildest rung's 0.2 % a
        // seed that spares one direction over a few hundred packets is ordinary.
        for r in rungs.iter().skip_while(|r| r.name != "clear").skip(1) {
            let (up, down) = (r.carried.up, r.carried.down);
            let dropped = [up.lost, up.overflowed, down.lost, down.overflowed]
                .into_iter()
                .fold(0_u64, u64::saturating_add);
            assert!(dropped > 0, "{}: the shaper dropped nothing: {r:?}", r.name);
        }
    }

    /// A capture target that produces no frame at all: the worker says so, and the receiver stops
    /// asking for refreshes no refresh can answer.
    ///
    /// The target is this test's own window ([`slopty-idle-window`]), on screen while the worker
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
        wait_for_marker(&ready, "the idle window").await;

        let (_guard, addr) = daemons(dir.path()).await;
        let (endpoint, worker) = dial(addr).await;
        let mut link = slopty_client::WorkerLink::start(worker);
        let mut events = link.events().unwrap();
        link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let target = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Listing { windows, .. })) => {
                    let found = windows.iter().find(|w| w.title == title);
                    break found.expect("the idle window in the listing").id;
                }
                _other => {}
            }
        };
        // Off screen before the stream opens: listed (the worker enumerates with
        // `onScreenWindowsOnly: false`), captured, and never drawing.
        std::fs::write(markers.join("hide"), b"").unwrap();
        wait_for_window_off_screen(&markers, target, Duration::from_secs(5)).await;

        link.send(ClientMsg::Screen(ScreenRequest::Open {
            target: CaptureTarget::Window(target),
            quality: Quality::default(),
        }))
        .await
        .unwrap();
        let (stream, codec) = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Opened {
                    stream,
                    codec,
                    ..
                })) => break (stream, codec),
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Closed { reason, .. })) => {
                    panic!("open failed: {reason}")
                }
                _other => {}
            }
        };
        let screen = link.screen(stream, codec);

        // The worker notices there is nothing to capture and says so, exactly as the app's canvas
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
            if let LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Source { state, .. })) = event
            {
                states.push(state);
                screen.set_source_live(state == SourceState::Live);
                if state == SourceState::Idle {
                    break;
                }
            }
        }
        assert_eq!(states.last(), Some(&SourceState::Idle), "the worker never called it idle");

        // Told that, the receiver gives up asking: whatever it sent before the hint, it sends no
        // more of them over the next stretch, and the count is inside the cap either way.
        // Quiescence observation window: verifies no frames and bounded refreshes over 3 s; no
        // event can be observed for silence.
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
            if let Ok(Some(LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Source {
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
        assert!(live, "the worker never took the idle hint back");
        assert!(back.frames > 0, "no picture after the window drew again: {back:?}");

        // Hiding again sticks: the markers are events the helper consumes, so a stale `hide`
        // cannot order the window out on the tick after every `show`, and a stale `show` cannot
        // undo this one. The worker's own window list is the evidence — not the stream, which by
        // now runs through the display-crop path and keeps sending whatever is on that patch of
        // desktop whether the window is there or not.
        std::fs::write(markers.join("hide"), b"").unwrap();
        // Polled, because the worker reuses an enumeration for `SHAREABLE_TTL`: one listing taken
        // just before the hide would still say the window is up.
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("a deadline inside the clock");
        let mut listed = None;
        let mut idle_again = false;
        while tokio::time::Instant::now() < deadline {
            // Poll interval: queries the worker window list every 500 ms until the cached
            // enumeration expires and reports on_screen=false.
            tokio::time::sleep(Duration::from_millis(500)).await;
            link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
            let windows = loop {
                match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                    LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Listing {
                        windows, ..
                    })) => {
                        break windows;
                    }
                    // The worker reports the source within a tick of the hide, which is while this
                    // poll is still running; taking it here is the difference between seeing it
                    // and throwing it away.
                    LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Source {
                        state, ..
                    })) => {
                        idle_again |= state == SourceState::Idle;
                    }
                    _other => {}
                }
            };
            listed = windows.into_iter().find(|w| w.title == title);
            if listed.as_ref().is_some_and(|w| !w.on_screen) {
                break;
            }
        }
        let listed = listed.unwrap_or_else(|| {
            panic!("the idle window listing never showed on_screen=false after waiting 10 s")
        });
        let state = std::fs::read_to_string(markers.join("state")).unwrap_or_default();
        assert!(
            !listed.on_screen,
            "the second hide did not stick after waiting 10 s: {listed:?}; helper: {state}"
        );

        // And the worker says so again. The old rule latched on "has ever encoded a frame", so a
        // window that drew and then went away stayed `Live` for the rest of the stream and the
        // receiver had only its refresh cap to protect it.
        idle_again |=
            wait_for_source(&mut events, SourceState::Idle, Duration::from_secs(10)).await;
        assert!(idle_again, "a window that drew and then hid was still reported live");

        std::fs::write(markers.join("quit"), b"").unwrap();
        let _stopped = helper.wait().await;
        drop(screen);
        link.close();
        close_endpoint(&endpoint).await;
    }

    /// [`bin`] for a binary whose package is not named after it, built every time rather than
    /// only when it is missing: this one is a test fixture that changes with the test, and a
    /// stale copy left beside the daemons would quietly test the previous version of it.
    fn bin_of(package: &str, name: &str) -> PathBuf {
        let worker = PathBuf::from(env!("CARGO_BIN_EXE_slopty-worker"));
        let path = worker.with_file_name(name);
        let release = worker.parent().is_some_and(|dir| dir.ends_with("release"));
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut build = std::process::Command::new(cargo);
        build.args(["build", "-p", package, "--bin", name]);
        if release {
            build.arg("--release");
        }
        let status = build.status().expect("run cargo");
        assert!(status.success(), "build {name}");
        path
    }

    /// Where both windows of the crop test open, in screen points from the bottom left: the same
    /// place, so the one ordered in second covers the first exactly.
    const CROP_ORIGIN: &str = "40,40";

    /// Wrap `binary` in a minimal application bundle under `dir` and return the executable
    /// inside it.
    ///
    /// The crop filter is built with
    /// `initWithDisplay:includingApplications:exceptingWindows:` on the target's owning
    /// application, so what it can show depends on what ScreenCaptureKit counts as a *different*
    /// application. A second copy of a bare executable is not one: it has no bundle identifier,
    /// and the framework treats it as the same application as the first. This gives the fixture
    /// its own identifier and signature, which is the only way to ask the question honestly.
    fn bundled(dir: &std::path::Path, binary: &std::path::Path, id: &str, name: &str) -> PathBuf {
        let app = dir.join(format!("{name}.app"));
        let macos = app.join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos).expect("the bundle directories");
        let executable = binary.file_name().expect("the helper's name");
        std::fs::copy(binary, macos.join(executable)).expect("copy the helper into the bundle");
        std::fs::write(
            app.join("Contents").join("Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>
	<string>{}</string>
	<key>CFBundleIdentifier</key>
	<string>{id}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>{name}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>1.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
"#,
                executable.to_string_lossy()
            ),
        )
        .expect("the bundle's Info.plist");
        std::fs::write(app.join("Contents").join("PkgInfo"), "APPL????").expect("PkgInfo");
        // Ad hoc, like `cargo xtask bundle` without an identity: enough for the window server
        // and ScreenCaptureKit to treat this as its own application.
        let signed = std::process::Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&app)
            .status()
            .expect("run codesign");
        assert!(signed.success(), "codesign {app:?}");
        macos.join(executable)
    }

    /// `datagrams` per hundred frames, 0 when no frame was sent.
    fn per_hundred(datagrams: u64, frames: u64) -> u64 {
        datagrams.saturating_mul(100).checked_div(frames).unwrap_or(0)
    }

    /// Mean brightness of a decoded picture: the average of its luma plane, 0-255.
    ///
    /// This is the only thing in these tests that looks at what was actually sent, and it looks
    /// at it as one number. A window repainting inside the crop moves it every tick; a
    /// rectangle holding nothing but the desktop does not move it at all.
    fn mean_luma(image: &slopty_codec::PixelBuffer) -> f64 {
        use objc2_core_video::{
            CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
            CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidthOfPlane,
            CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
        };

        let buffer = image.as_cv();
        // SAFETY: the buffer came from the decoder and is not locked; the flags are the
        // documented read-only lock (CoreVideo, `CVPixelBufferLockBaseAddress`).
        let locked =
            unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
        assert_eq!(locked, 0, "lock the decoded frame");
        let base = CVPixelBufferGetBaseAddressOfPlane(buffer, 0).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0);
        let width = CVPixelBufferGetWidthOfPlane(buffer, 0);
        let height = CVPixelBufferGetHeightOfPlane(buffer, 0);
        let mut sum = 0_u64;
        for y in 0..height {
            for x in 0..width {
                // SAFETY: plane 0 is locked and mapped, `y < height` and `x < width <= stride`,
                // so the offset is inside it.
                let cell = unsafe { base.add(y.saturating_mul(stride).saturating_add(x)) };
                // SAFETY: as above; the byte is initialised, this is the decoded picture.
                let luma = unsafe { cell.read() };
                sum = sum.saturating_add(u64::from(luma));
            }
        }
        // SAFETY: the same buffer and flags this function locked above.
        let unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
        assert_eq!(unlocked, 0, "unlock the decoded frame");
        let pixels = width.saturating_mul(height);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a sum of bytes over one small picture, far below 2^53"
        )]
        let mean = sum as f64 / pixels as f64;
        if pixels == 0 { 0.0 } else { mean }
    }

    /// Watch decoded frames and record when each arrived and how bright it was, until the
    /// returned sender is dropped.
    fn watch_luma(
        handle: &slopty_client::ScreenHandle,
    ) -> (tokio::task::JoinHandle<Vec<(std::time::Instant, f64)>>, tokio::sync::oneshot::Sender<()>)
    {
        let mut frames = handle.frames();
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut seen = Vec::new();
            loop {
                tokio::select! {
                    _stop = &mut stopped => break,
                    changed = frames.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let picture = frames.borrow_and_update().clone();
                        if let Some(picture) = picture {
                            seen.push((
                                std::time::Instant::now(),
                                mean_luma(&picture.frame.image),
                            ));
                        }
                    }
                }
            }
            seen
        });
        (task, stop)
    }

    /// Mean brightness over `samples`, 0 when there are none.
    fn luma_level(samples: &[(std::time::Instant, f64)]) -> f64 {
        let total: f64 = samples.iter().map(|&(_at, luma)| luma).sum();
        #[expect(clippy::cast_precision_loss, reason = "a handful of samples")]
        let count = samples.len() as f64;
        if samples.is_empty() { 0.0 } else { total / count }
    }

    /// How much the picture moved over `samples`: the difference between the brightest and the
    /// dimmest of them, 0 when there are fewer than two.
    fn luma_spread(samples: &[(std::time::Instant, f64)]) -> f64 {
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for &(_at, luma) in samples {
            lo = lo.min(luma);
            hi = hi.max(luma);
        }
        if samples.len() < 2 { 0.0 } else { hi - lo }
    }

    /// How many windows the accessibility API says an application has right now, or `None`
    /// when it will not answer (no Accessibility grant, or the process is gone).
    ///
    /// The attribute name is spelled out because there is nothing to import: the `kAX*`
    /// constants are `#define kAXWindowsAttribute CFSTR("AXWindows")` macros in
    /// `AXNotificationConstants.h` and `AXAttributeConstants.h`, so no symbol is exported and
    /// no objc2 binding can offer a static for them. This is a measurement, and the spelling
    /// stays inside it.
    fn ax_window_count(pid: i32) -> Option<usize> {
        use objc2_application_services::{AXError, AXUIElement};
        use objc2_core_foundation::{CFArray, CFRetained, CFString, CFType};

        // SAFETY: the documented constructor for an application element; it returns +1.
        let app = unsafe { AXUIElement::new_application(pid) };
        let attribute = CFString::from_str("AXWindows");
        let mut value: *const CFType = std::ptr::null();
        // SAFETY: `app` is a live element and `value` is a valid out-pointer for one
        // `CFTypeRef` (Accessibility, `AXUIElementCopyAttributeValue`).
        let status =
            unsafe { app.copy_attribute_value(&attribute, std::ptr::NonNull::from(&mut value)) };
        if status != AXError::Success {
            return None;
        }
        let value = std::ptr::NonNull::new(value.cast_mut())?;
        // SAFETY: the call above returned success, so it stored a +1 reference here.
        let value = unsafe { CFRetained::from_raw(value) };
        // SAFETY: `AXWindows` is documented to be an array of elements.
        let windows: CFRetained<CFArray> = unsafe { CFRetained::cast_unchecked(value) };
        Some(windows.count().try_into().unwrap_or(usize::MAX))
    }

    /// What sits in the rectangle behind the target while the crop test runs.
    #[derive(Clone, Copy, Debug)]
    enum Behind {
        /// The desktop, and nothing else. The control: whatever the counters do here is what
        /// they do without any window in the rectangle at all.
        Nothing,
        /// A second window of the same executable. ScreenCaptureKit gives a bare binary no
        /// bundle identifier, so both processes are one application to the crop filter.
        SameApplication,
        /// The same binary inside its own signed bundle, which the framework does count as
        /// another application — the case the filter is supposed to exclude.
        AnotherApplication,
    }

    /// What one hide looked like, all of it read from the worker's own counters and the client's
    /// own pictures. Every field is printed with the run and belongs to the record in
    /// MEASUREMENTS.md; the assertions use the few that carry a rule.
    #[derive(Debug)]
    #[expect(dead_code, reason = "the whole record is printed, and read from the test output")]
    struct HideRun {
        /// Marker written → AppKit reports the window ordered out.
        order_ms: u128,
        /// Ordered out → the worker has left the crop path.
        swap_ms: u128,
        /// Crop frames sent after the window was *asked* to go, counted from a reading taken
        /// before the request: nothing between the two can escape it. This is the one the guard
        /// is written on, and it includes the frames the still-visible window earns while the
        /// helper takes its own tick to act.
        after_hide: u64,
        /// The same from a reading taken once AppKit reported the window gone. Tighter, and
        /// reported rather than asserted: the reading is a round trip to the control socket, so
        /// frames served in that gap are missing from it.
        after_order: u64,
        /// Datagrams per hundred crop frames while the target is up and repainting: what a
        /// rectangle with a flashing window in it costs to encode.
        bits_visible: u64,
        /// The same over the frames sent after the window was ordered out. A rectangle holding
        /// only wallpaper is the same picture every time and encodes to almost nothing, so this
        /// is what says whether anything was still being drawn in the crop.
        bits_after_order: u64,
        /// Frames encoded in the two seconds after the swap, with the window still away.
        while_away: u64,
        /// Frames the guard threw away over the whole hide.
        withheld: u64,
        /// Frames held on the accessibility API's word alone, before the window list agreed:
        /// where the hide is caught first once the watch is on (`ScreenStats::suspected`).
        suspected: u64,
        /// Accessibility notifications the hide raised on the worker (`ScreenStats::suspicions`).
        suspicions: u64,
        /// How far the decoded picture's brightness moved while the target was up and drawing:
        /// what a window repainting inside the crop looks like from the client, as one number.
        luma_visible: f64,
        /// The same over the pictures decoded between the window being ordered out and the worker
        /// leaving the crop. The window vanishing is itself the largest change in that window,
        /// so this is large in every case and says nothing on its own.
        luma_after_order: f64,
        /// The spread over the *last* pictures of that window, once the target has gone from the
        /// rectangle. This is the one that answers the question: near zero means the crop then
        /// held something that never changed — the desktop — and not a window still being drawn.
        luma_tail: f64,
        /// How bright those last pictures were. The control run's value is what the desktop
        /// behind the target looks like, so a case whose value matches it was showing the
        /// desktop and not the window behind.
        luma_tail_level: f64,
        /// How many of those pictures there were, so a spread of zero can be told from no data.
        samples_after_order: usize,
        /// How many of them were unlike anything decoded while the target was visible (more than
        /// 8 luma levels from every one of those): a picture of the gap. Pictures *of the
        /// target* decoded after the order are frames sent before it and are not a leak, however
        /// many the client's decoder lets out at once.
        foreign_after_order: usize,
        /// Whether the picture came back when the window did.
        recovered: bool,
    }

    /// Drive one window onto the display-crop path, hide it with `behind` in the rectangle, and
    /// report what the worker did. The assertions belong to the callers, which differ only in what
    /// is behind the target.
    async fn crop_hide(behind: Behind) -> HideRun {
        let dir = tempfile::tempdir().unwrap();
        let helper_bin = bin_of("slopty-e2e", "slopty-idle-window");
        // The thing that must never reach the client, in the rectangle the crop covers. It
        // repaints throughout: without something changing there, ScreenCaptureKit has no new
        // frame for the rectangle once the target goes, and every assertion below about what is
        // not sent would pass for free.
        let backdrop_markers = dir.path().join("backdrop");
        std::fs::create_dir_all(&backdrop_markers).unwrap();
        let backdrop_bin = match behind {
            Behind::Nothing => None,
            Behind::SameApplication => Some({
                let copy = dir.path().join("slopty-backdrop-window");
                std::fs::copy(&helper_bin, &copy).expect("copy the helper");
                copy
            }),
            Behind::AnotherApplication => {
                Some(bundled(dir.path(), &helper_bin, "dev.slopty.test.backdrop", "SloptyBackdrop"))
            }
        };
        let mut backdrop = match &backdrop_bin {
            None => None,
            Some(path) => {
                let child = Command::new(path)
                    .arg(&backdrop_markers)
                    .arg(format!("slopty backdrop {}", std::process::id()))
                    .arg(CROP_ORIGIN)
                    .kill_on_drop(true)
                    .spawn()
                    .expect("spawn the backdrop window");
                wait_for_marker(&backdrop_markers.join("ready"), "the backdrop window").await;
                Some(child)
            }
        };

        let markers = dir.path().join("markers");
        std::fs::create_dir_all(&markers).unwrap();
        let title = format!("slopty crop {}", std::process::id());
        let mut helper = Command::new(&helper_bin)
            .arg(&markers)
            .arg(&title)
            .arg(CROP_ORIGIN)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn the idle window");
        wait_for_marker(&markers.join("ready"), "the idle window").await;

        let (_guard, addr) = daemons(dir.path()).await;
        let ctl = dir.path().join("worker.sock");
        let (endpoint, worker) = dial(addr).await;
        let mut link = slopty_client::WorkerLink::start(worker);
        let mut events = link.events().unwrap();
        link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let target = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Listing { windows, .. })) => {
                    let found = windows.iter().find(|w| w.title == title);
                    break found.expect("the crop window in the listing").id;
                }
                _other => {}
            }
        };
        link.send(ClientMsg::Screen(ScreenRequest::Open {
            target: CaptureTarget::Window(target),
            quality: Quality::default(),
        }))
        .await
        .unwrap();
        let (stream, codec) = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Opened {
                    stream,
                    codec,
                    ..
                })) => break (stream, codec),
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Closed { reason, .. })) => {
                    panic!("open failed: {reason}")
                }
                _other => {}
            }
        };
        let screen = link.screen(stream, codec);

        // Visible and unobstructed, so the worker serves it as a crop of its display.
        let cropping = wait_for_stats(&ctl, Duration::from_secs(15), |s| s.on_crop)
            .await
            .expect("the worker never took the display-crop path after waiting 15 s");

        // Wait until the crop path has produced cropped frames while the window is drawing.
        let drawing =
            wait_for_stats(&ctl, Duration::from_secs(10), |s| s.cropped > cropping.cropped)
                .await
                .unwrap_or_else(|| {
                    panic!(
                        "cropped frames did not increment above {} after waiting 10 s",
                        cropping.cropped
                    )
                });
        let bits_visible = per_hundred(
            drawing.datagrams.saturating_sub(cropping.datagrams),
            drawing.cropped.saturating_sub(cropping.cropped),
        );
        let (luma, stop_luma) = watch_luma(&screen);
        let watching_from = std::time::Instant::now();
        // Wait for at least one decoded frame to arrive while the window is drawing.
        let mut frames = screen.frames();
        let frame_deadline =
            tokio::time::Instant::now().checked_add(Duration::from_secs(10)).expect("deadline");
        loop {
            tokio::time::timeout_at(frame_deadline, frames.changed())
                .await
                .unwrap_or_else(|_| {
                    panic!("received no frame while window was drawing after waiting 10 s")
                })
                .expect("screen frames channel open");
            if frames.borrow_and_update().is_some() {
                break;
            }
        }

        // The counter as it stands before anything is asked. Everything the crop serves from
        // here on is measured against this, because a reading taken *after* the order is one
        // round trip to the control socket late, and the frames served in that gap would go
        // uncounted — a slow reply would let any number of them through while the guard read
        // zero.
        let before_hide = wait_for_stats(&ctl, Duration::from_secs(5), |_s| true)
            .await
            .expect("the stream is still live after waiting 5 s");
        std::fs::write(markers.join("hide"), b"").unwrap();
        let hidden_at = std::time::Instant::now();
        // When the window was really ordered out, as AppKit saw it: the marker is only a
        // request, and everything measured below is measured from the act, not the asking.
        let ordered_out = loop {
            if std::fs::read_to_string(markers.join("state"))
                .is_ok_and(|s| s.starts_with("hide visible=false"))
            {
                break std::time::Instant::now();
            }
            assert!(
                hidden_at.elapsed() < Duration::from_secs(5),
                "the helper never hid after waiting 5 s"
            );
            // Poll interval: checks helper state file every 5 ms until AppKit orders out the
            // window.
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        let at_order = wait_for_stats(&ctl, Duration::from_secs(5), |_s| true)
            .await
            .expect("the stream is still live after waiting 5 s");
        let swapped = wait_for_stats(&ctl, Duration::from_secs(10), |s| !s.on_crop)
            .await
            .expect("a hidden window stayed on the crop path: it streams what is behind it after waiting 10 s");
        let left_crop_at = std::time::Instant::now();
        let swap_ms = left_crop_at.saturating_duration_since(ordered_out).as_millis();
        let order_ms = ordered_out.saturating_duration_since(hidden_at).as_millis();

        // Give the client time to decode what the worker sent before the swap, then read the
        // pictures. The window that matters runs from the order to the swap; frames decoded
        // after that are the same ones, arriving late, so the cut is by arrival with a margin
        // and the three cases are compared against each other rather than a threshold.
        // Drains in-flight frames: gives client decoder time to process frames sent before swap;
        // stream carries no path-swap marker.
        tokio::time::sleep(Duration::from_millis(800)).await;
        let _stopped = stop_luma.send(());
        let seen = luma.await.expect("the luma watcher");
        let visible: Vec<_> = seen
            .iter()
            .filter(|(at, _l)| *at > watching_from && *at < ordered_out)
            .copied()
            .collect();
        let during: Vec<_> = seen
            .iter()
            .filter(|(at, _l)| {
                *at > ordered_out
                    && *at
                        < left_crop_at
                            .checked_add(Duration::from_millis(500))
                            .expect("a deadline inside the clock")
            })
            .copied()
            .collect();

        // Nothing more is captured while the window is away: not from the crop (the rectangle
        // holds the backdrop, which is still repainting) and not from the window filter (there
        // is no window). The count is the worker's, so it does not race the client decoding the
        // frames that were legitimately sent while the window was still up.
        // Quiescence observation window: proves nothing is captured while window is away; no event
        // can be observed for silence.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let after = wait_for_stats(&ctl, Duration::from_secs(5), |_s| true)
            .await
            .expect("the stream is still live after waiting 5 s");
        assert!(!after.on_crop, "a hidden window went back onto the crop path: {after:?}");

        // Showing it again brings the picture back, with no help from the client.
        let quiet = after.encoded;
        std::fs::write(markers.join("show"), b"").unwrap();
        let back = wait_for_stats(&ctl, Duration::from_secs(15), |s| s.encoded > quiet).await;

        std::fs::write(markers.join("quit"), b"").unwrap();
        let _stopped = helper.wait().await;
        if let Some(child) = &mut backdrop {
            std::fs::write(backdrop_markers.join("quit"), b"").unwrap();
            let _backdrop_stopped = child.wait().await;
        }
        drop(screen);
        link.close();
        close_endpoint(&endpoint).await;

        let run = HideRun {
            order_ms,
            swap_ms,
            after_hide: swapped.cropped.saturating_sub(before_hide.cropped),
            after_order: swapped.cropped.saturating_sub(at_order.cropped),
            bits_visible,
            bits_after_order: per_hundred(
                swapped.datagrams.saturating_sub(at_order.datagrams),
                swapped.cropped.saturating_sub(at_order.cropped),
            ),
            while_away: after.encoded.saturating_sub(swapped.encoded),
            luma_visible: luma_spread(&visible),
            luma_after_order: luma_spread(&during),
            luma_tail: luma_spread(during.get(during.len().saturating_sub(4)..).unwrap_or(&[])),
            luma_tail_level: luma_level(
                during.get(during.len().saturating_sub(4)..).unwrap_or(&[]),
            ),
            samples_after_order: during.len(),
            foreign_after_order: during
                .iter()
                .filter(|&&(_at, luma)| {
                    visible.iter().all(|&(_at, seen)| (luma - seen).abs() > FOREIGN_LUMA)
                })
                .count(),
            withheld: swapped.withheld.saturating_sub(cropping.withheld),
            suspected: swapped.suspected.saturating_sub(cropping.suspected),
            suspicions: swapped.suspicions.saturating_sub(cropping.suspicions),
            recovered: back.is_some(),
        };
        eprintln!("{behind:?}: {run:?}");
        run
    }

    /// What a stream must never show: a window on the display-crop path that is hidden stops
    /// being served from that rectangle, so the viewer sees the picture stop rather than what
    /// was behind it. Gated on `SLOPTY_SCREEN_E2E`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_hidden_window_stops_being_served_from_its_crop() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let run = crop_hide(Behind::SameApplication).await;

        // A ceiling for a worker without the accessibility watch, not the rule, and counted from
        // before the hide was even asked for so that nothing in between escapes it. Two things
        // fill it there: the helper's own tick before it orders the window out, and the ~260 ms
        // in which every way of asking the WindowServer still says the window is on screen
        // (MEASUREMENTS.md, "how late a hide is"). At 60 Hz that is around thirty frames; sixty
        // is what a regression would have to beat. With the watch on it is 0–2
        // (MEASUREMENTS.md, "the accessibility hide watch"), and `assert_the_gap_reached_no_one`
        // holds it there. The statement that no frame gets through once the worker does know is
        // the unit test (`a_frame_captured_while_the_target_is_hidden_is_withheld`), not this.
        assert!(run.after_hide <= 60, "crop frames sent after the window was asked to go: {run:?}");
        assert_eq!(run.while_away, 0, "frames were still being made for a hidden window: {run:?}");
        assert!(run.recovered, "no picture after the window came back: {run:?}");
        assert_the_gap_reached_no_one(&run);
    }

    /// How far, in luma levels, a decoded picture must sit from every picture of the visible
    /// target to count as a picture of something else.
    const FOREIGN_LUMA: f64 = 8.0;

    /// How long the beat measurement watches a quiet stream.
    const QUIET_FOR: Duration = Duration::from_secs(60);

    /// The heartbeat is what a receiver has to go on while nothing is being drawn: it must
    /// arrive inside `STALL_GAP` (50 ms) or the receiver calls the silence a stall, and the worker
    /// promises one every `HEARTBEAT_AFTER` (25 ms). This watches a stream whose target draws
    /// nothing for a minute and reads, from the worker's own counters, how far apart the beats
    /// actually were and how long the window-geometry call in the same loop took.
    /// Gated on `SLOPTY_SCREEN_E2E`.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_heartbeat_keeps_its_cadence_on_a_quiet_stream() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let markers = dir.path().join("markers");
        std::fs::create_dir_all(&markers).unwrap();
        let title = format!("slopty quiet {}", std::process::id());
        let mut helper = Command::new(bin_of("slopty-e2e", "slopty-idle-window"))
            .arg(&markers)
            .arg(&title)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn the idle window");
        wait_for_marker(&markers.join("ready"), "the idle window").await;

        let (_guard, addr) = daemons(dir.path()).await;
        let ctl = dir.path().join("worker.sock");
        let (endpoint, worker) = dial(addr).await;
        let mut link = slopty_client::WorkerLink::start(worker);
        let mut events = link.events().unwrap();
        link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let target = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Listing { windows, .. })) => {
                    let found = windows.iter().find(|w| w.title == title);
                    break found.expect("the window in the listing").id;
                }
                _other => {}
            }
        };
        // Order it out *before* the stream opens, so the minute that follows is the steady
        // state: a stream carrying beats and nothing else, with no path swap in it. This is the
        // case the beat exists for, and the one claude/stalls found the worker going quiet in.
        std::fs::write(markers.join("hide"), b"").unwrap();
        wait_for_window_off_screen(&markers, target, Duration::from_secs(5)).await;

        link.send(ClientMsg::Screen(ScreenRequest::Open {
            target: CaptureTarget::Window(target),
            quality: Quality::default(),
        }))
        .await
        .unwrap();
        let (stream, codec) = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Opened {
                    stream,
                    codec,
                    ..
                })) => break (stream, codec),
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Closed { reason, .. })) => {
                    panic!("open failed: {reason}")
                }
                _other => {}
            }
        };
        let screen = link.screen(stream, codec);

        // Measurement window: observes the heartbeat cadence over the full quiet duration; no event
        // to wait for.
        tokio::time::sleep(QUIET_FOR).await;
        let stats = wait_for_stats(&ctl, Duration::from_secs(5), |_s| true)
            .await
            .expect("the stream is still live after waiting 5 s");
        let client = screen.stats();
        eprintln!(
            "beat: gap p50 {} / p95 {} / max {} µs, worst ever {} µs over {} beats; bounds \
             p50 {} / p95 {} / max {} µs over {}; client saw {} stalls, {} ms stalled",
            stats.beat_gap.p50_us,
            stats.beat_gap.p95_us,
            stats.beat_gap.max_us,
            stats.beat_gap_worst_us,
            stats.heartbeats,
            stats.bounds.p50_us,
            stats.bounds.p95_us,
            stats.bounds.max_us,
            stats.bounds.n,
            client.stalls,
            client.stalled_ms,
        );

        // What the beat promises the receiver: typically well inside the gap it would otherwise
        // call a stall, and no stall actually counted over the minute. The worst single gap is
        // deliberately not asserted — it is still about 75 ms once a minute, and the cause is
        // other window-server work on the runtime rather than this loop (MEASUREMENTS.md, "the
        // beat behind the geometry call").
        let gap = Duration::from_micros(stats.beat_gap.p95_us);
        assert!(
            gap < slopty_media::STALL_GAP,
            "the beat's p95 is past the gap the receiver calls a stall: {stats:?}"
        );
        assert_eq!(client.stalls, 0, "the receiver counted a stall on a quiet loopback stream");

        std::fs::write(markers.join("quit"), b"").unwrap();
        let _stopped = helper.wait().await;
        drop(screen);
        link.close();
        close_endpoint(&endpoint).await;
    }

    /// How late each way of noticing a hide is, measured against the same order-out.
    ///
    /// The display-crop path leaves a window in the crop until it learns the window has gone,
    /// and CoreGraphics does not say so for ~270 ms (MEASUREMENTS.md, "how late a hide is").
    /// This asks whether the accessibility API knows sooner, since it is the one signal that is
    /// public, documented and not a poll of the same window list. Gated on `SLOPTY_SCREEN_E2E`,
    /// and it reports rather than asserts a threshold: the number is the point.
    #[tokio::test(flavor = "multi_thread")]
    async fn how_late_each_way_of_noticing_a_hide_is() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        // SAFETY: the documented no-argument query; it takes and returns nothing owned.
        let trusted = unsafe { objc2_application_services::AXIsProcessTrusted() };
        let dir = tempfile::tempdir().unwrap();
        let markers = dir.path().join("markers");
        std::fs::create_dir_all(&markers).unwrap();
        let title = format!("slopty late {}", std::process::id());
        let mut helper = Command::new(bin_of("slopty-e2e", "slopty-idle-window"))
            .arg(&markers)
            .arg(&title)
            .arg(CROP_ORIGIN)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn the idle window");
        wait_for_marker(&markers.join("ready"), "the idle window").await;
        let pid = i32::try_from(helper.id().expect("the helper's pid")).expect("a pid");

        // The window as the worker would find it, so both signals are asked about the same one.
        let (_guard, addr) = daemons(dir.path()).await;
        let (endpoint, worker) = dial(addr).await;
        let mut link = slopty_client::WorkerLink::start(worker);
        let mut events = link.events().unwrap();
        link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let target = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Listing { windows, .. })) => {
                    let found = windows.iter().find(|w| w.title == title);
                    break found.expect("the window in the listing").id;
                }
                _other => {}
            }
        };
        let windows_before = ax_window_count(pid);

        std::fs::write(markers.join("hide"), b"").unwrap();
        let asked = std::time::Instant::now();
        let ordered_out = loop {
            if std::fs::read_to_string(markers.join("state"))
                .is_ok_and(|s| s.starts_with("hide visible=false"))
            {
                break std::time::Instant::now();
            }
            assert!(
                asked.elapsed() < Duration::from_secs(5),
                "the helper never hid after waiting 5 s"
            );
            // Poll interval: checks helper state file every 2 ms until AppKit orders out the
            // window.
            tokio::time::sleep(Duration::from_millis(2)).await;
        };

        // Both polled as fast as they can be answered, from the same thread, so neither is
        // charged for the other. The blocking sleep is deliberate: a timer would round both to
        // its own resolution.
        let (mut ax_ms, mut cg_ms) = (None, None);
        while ax_ms.is_none() || cg_ms.is_none() {
            let elapsed = ordered_out.elapsed().as_millis();
            if ax_ms.is_none() && ax_window_count(pid) < windows_before {
                ax_ms = Some(elapsed);
            }
            if cg_ms.is_none() && !slopty_capture::window_on_screen(target) {
                cg_ms = Some(elapsed);
            }
            if ordered_out.elapsed() > Duration::from_secs(3) {
                break;
            }
            // Poll interval: samples AX and CoreGraphics every 2 ms to measure detection latency
            // without timer rounding.
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        eprintln!(
            "late: accessibility {ax_ms:?} ms, core graphics {cg_ms:?} ms after the order \
             (AXIsProcessTrusted = {trusted}, windows before = {windows_before:?})"
        );

        std::fs::write(markers.join("quit"), b"").unwrap();
        let _stopped = helper.wait().await;
        link.close();
        close_endpoint(&endpoint).await;
    }

    /// Whether a pop-up of the application counts. Autocomplete lists, tooltips and menus are
    /// windows of the process too — borderless, non-activating panels at the pop-up menu
    /// level — and an editor opens and closes them constantly. If the accessibility watch
    /// counted their going as a suspicion, every one would freeze the stream for the hold, so
    /// this opens and orders out exactly such a panel below the target (never over it, so the
    /// occlusion rule stays out of the picture) and reads the worker's counters: no suspicion,
    /// one sibling, the stream flowing throughout and on the crop afterwards. Gated on
    /// `SLOPTY_SCREEN_E2E`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_popup_of_the_application_closing_raises_no_suspicion() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        another_window_of_the_application_goes("popup", "unpopup").await;
    }

    /// What a sibling window costs. The accessibility watch cannot name a window, but it can
    /// match the target to its element once, so another titled window of the same process
    /// going away is not a suspicion. It is still the event ScreenCaptureKit stalls on
    /// (MEASUREMENTS.md, "a sibling window closing stalls the crop"): without the round trip
    /// through the window filter the framework delivers nothing more, ever. This opens a second
    /// window of the helper's own process beside the target, orders it out while the target
    /// streams on the crop path, and reads the worker's counters: no suspicion, one sibling,
    /// nothing held or withheld, the stream flowing throughout and back on the crop. Opening
    /// the sibling must raise nothing either. Gated on `SLOPTY_SCREEN_E2E`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sibling_window_closing_keeps_the_stream_flowing() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        another_window_of_the_application_goes("sibling", "unsibling").await;
    }

    /// The driver of the two tests above: a crop-path stream on the idle window, the helper's
    /// `open` marker, then its `close` marker, and the assertions that neither was taken for
    /// the target and the stream never stopped.
    async fn another_window_of_the_application_goes(open: &str, close: &str) {
        let dir = tempfile::tempdir().unwrap();
        let markers = dir.path().join("markers");
        std::fs::create_dir_all(&markers).unwrap();
        let title = format!("slopty {open} {}", std::process::id());
        let mut helper = Command::new(bin_of("slopty-e2e", "slopty-idle-window"))
            .arg(&markers)
            .arg(&title)
            .arg(CROP_ORIGIN)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn the idle window");
        wait_for_marker(&markers.join("ready"), "the idle window").await;

        let (_guard, addr) = daemons(dir.path()).await;
        let ctl = dir.path().join("worker.sock");
        let (endpoint, worker) = dial(addr).await;
        let mut link = slopty_client::WorkerLink::start(worker);
        let mut events = link.events().unwrap();
        link.send(ClientMsg::Screen(ScreenRequest::List)).await.unwrap();
        let target = loop {
            match tokio::time::timeout(STEP, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Listing { windows, .. })) => {
                    let found = windows.iter().find(|w| w.title == title);
                    break found.expect("the window in the listing").id;
                }
                _other => {}
            }
        };
        link.send(ClientMsg::Screen(ScreenRequest::Open {
            target: CaptureTarget::Window(target),
            quality: Quality::default(),
        }))
        .await
        .unwrap();
        let cropping = wait_for_stats(&ctl, Duration::from_secs(15), |s| s.on_crop)
            .await
            .expect("the worker never took the display-crop path after waiting 15 s");
        let flowing = wait_for_stats(&ctl, Duration::from_secs(10), |s| {
            s.encoded > cropping.encoded.saturating_add(10)
        })
        .await
        .expect("no frames flowed on the crop after waiting 10 s");
        assert_eq!(
            (flowing.suspicions, flowing.siblings),
            (0, 0),
            "a notification before anything happened: {flowing:?}"
        );

        // The other window appears: a window created is not a window gone.
        std::fs::write(markers.join(open), b"").unwrap();
        wait_for_marker_state(&markers, &format!("{open} visible=true")).await;
        // Observation window: a suspicion raised by the arrival would land inside the hold;
        // one second is comfortably past it.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let opened = wait_for_stats(&ctl, Duration::from_secs(5), |_s| true)
            .await
            .expect("the stream is still live after waiting 5 s");
        assert_eq!(
            (opened.suspicions, opened.siblings),
            (0, 0),
            "opening another window of the application was taken for one going: {opened:?}"
        );
        assert!(
            opened.encoded > flowing.encoded,
            "the stream stopped when {open} opened: {opened:?}"
        );

        // The other window goes: one sibling, no suspicion, a round trip through the window
        // filter that the stream flows straight through.
        std::fs::write(markers.join(close), b"").unwrap();
        wait_for_marker_state(&markers, &format!("{close} visible=false")).await;
        let gone_at = std::time::Instant::now();
        let heard = wait_for_stats(&ctl, Duration::from_secs(3), |s| s.siblings > 0)
            .await
            .expect("the order-out was not heard within 3 s");
        let flowing_again = wait_for_stats(&ctl, Duration::from_secs(3), |s| {
            s.encoded > heard.encoded.saturating_add(10)
        })
        .await
        .expect("the stream did not keep flowing after another window went (within 3 s)");
        let flowing_ms = gone_at.elapsed().as_millis();
        // Settling window: a late suspicion, hold or withhold would show inside it.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let after = wait_for_stats(&ctl, Duration::from_secs(5), |_s| true)
            .await
            .expect("the stream is still live after waiting 5 s");

        std::fs::write(markers.join("quit"), b"").unwrap();
        let _stopped = helper.wait().await;
        link.close();
        close_endpoint(&endpoint).await;

        eprintln!(
            "{close}: ten more frames {flowing_ms} ms after the order-out, siblings {}, \
             suspicions {}, held {}, withheld {}, on the crop {} ({after:?})",
            after.siblings, after.suspicions, after.suspected, after.withheld, after.on_crop,
        );
        assert_eq!(after.siblings, 1, "one order-out, one sibling: {after:?}");
        assert_eq!(after.suspicions, 0, "another window going was taken for the target: {after:?}");
        assert_eq!((after.suspected, after.withheld), (0, 0), "frames kept back: {after:?}");
        assert!(
            after.encoded > flowing_again.encoded,
            "the stream did not keep flowing: {after:?}"
        );
        assert!(after.on_crop, "the stream is not on the crop after {close}: {after:?}");
        assert!(
            flowing_ms <= 1_000,
            "the stream took {flowing_ms} ms for ten more frames after {close}: {after:?}"
        );
    }

    /// Poll the helper's `state` file until it starts with `want`.
    async fn wait_for_marker_state(markers: &std::path::Path, want: &str) {
        let asked = std::time::Instant::now();
        loop {
            if std::fs::read_to_string(markers.join("state")).is_ok_and(|s| s.starts_with(want)) {
                return;
            }
            assert!(
                asked.elapsed() < Duration::from_secs(5),
                "the helper never reported `{want}` within 5 s"
            );
            // Poll interval: checks the helper's state file every 5 ms.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// The control for the two tests around it: the same hide with nothing behind the target.
    /// Whatever the counters do here they do for an empty rectangle, so it is the only thing
    /// that makes a number from the other two mean anything. Gated on `SLOPTY_SCREEN_E2E`.
    #[tokio::test(flavor = "multi_thread")]
    async fn what_a_crop_shows_of_an_empty_rectangle() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let run = crop_hide(Behind::Nothing).await;
        assert_eq!(run.while_away, 0, "frames were still being made for a hidden window: {run:?}");
        assert!(run.recovered, "no picture after the window came back: {run:?}");
        assert_the_gap_reached_no_one(&run);
    }

    /// What reaches the client of the ~260 ms between AppKit ordering the window out and the
    /// window list admitting it: nothing, because the accessibility watch heard the order, the
    /// crop's frames were held (`suspected`) and the stream moved to the window filter, which
    /// has no frame for a hidden window; the pictures decoded in that gap are at most the one
    /// or two already in flight. On a worker without accessibility trust there is no watch, the
    /// crop runs on through the gap, and the question becomes what it carried: black, and
    /// still — the answer measured before the watch existed, kept as the fallback so the test
    /// says something either way.
    fn assert_the_gap_reached_no_one(run: &HideRun) {
        if run.suspicions == 0 {
            eprintln!(
                "no accessibility hide watch on the worker: asserting what the crop sent instead"
            );
            assert_dark_and_still(run);
            return;
        }
        assert!(run.suspected >= 1, "the watch fired but held nothing: {run:?}");
        assert!(run.after_order <= 2, "crop frames sent after the window was ordered out: {run:?}");
        assert!(
            run.foreign_after_order <= 2,
            "pictures of the gap after the order reached the client: {run:?}"
        );
    }

    /// What the crop is sending by the time the window has gone from it, whatever is behind:
    /// pictures that are black and that stop changing. The three cases differ only in what sits
    /// in the rectangle, so this holding in all of them is the answer to what the crop can show.
    fn assert_dark_and_still(run: &HideRun) {
        assert!(run.samples_after_order >= 4, "too few pictures decoded to say anything: {run:?}");
        assert!(
            run.luma_tail < 5.0,
            "the crop was still changing once the window had gone from it: {run:?}"
        );
        assert!(
            run.luma_tail_level < 5.0,
            "the crop was showing something once the window had gone from it: {run:?}"
        );
    }

    /// The same hide with **another application** behind the target. The crop filter is
    /// `initWithDisplay:includingApplications:exceptingWindows:` on the target's owning
    /// application, so the question this answers is whether that scope is real: during the
    /// ~270 ms in which nobody can tell the window has gone, does the crop carry the other
    /// application's window or not? Gated on `SLOPTY_SCREEN_E2E`.
    #[tokio::test(flavor = "multi_thread")]
    async fn what_a_crop_shows_of_another_application() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let run = crop_hide(Behind::AnotherApplication).await;
        assert_eq!(run.while_away, 0, "frames were still being made for a hidden window: {run:?}");
        assert!(run.recovered, "no picture after the window came back: {run:?}");
        // The ruling: the other application's window is repainting in that rectangle for the
        // whole of the gap, and none of it reaches the client — held by the watch, or black
        // without one.
        assert_the_gap_reached_no_one(&run);
    }

    /// Poll the worker's control socket until one live stream's counters satisfy `want`.
    async fn wait_for_stats(
        ctl: &std::path::Path,
        within: Duration,
        want: impl Fn(&slopty_worker::screen::ScreenStats) -> bool,
    ) -> Option<slopty_worker::screen::ScreenStats> {
        let deadline =
            tokio::time::Instant::now().checked_add(within).expect("a deadline inside the clock");
        while tokio::time::Instant::now() < deadline {
            for summary in screens(ctl).await {
                if want(&summary.stats) {
                    return Some(summary.stats);
                }
            }
            // Poll interval: queries the worker control socket for screens stats every 50 ms.
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    /// The live streams as `slopty worker screens` reads them.
    async fn screens(ctl: &std::path::Path) -> Vec<slopty_worker::screen::ScreenSummary> {
        use tokio::io::AsyncWriteExt as _;

        let Ok(mut stream) = tokio::net::UnixStream::connect(ctl).await else {
            return Vec::new();
        };
        if stream.write_all(b"{\"cmd\":\"screens\"}\n").await.is_err() {
            return Vec::new();
        }
        let mut line = String::new();
        if BufReader::new(stream).read_line(&mut line).await.is_err() {
            return Vec::new();
        }
        let reply: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        let live = reply.get("live").cloned().unwrap_or_default();
        serde_json::from_value(live).unwrap_or_default()
    }

    /// Wait for a helper to leave a marker file.
    async fn wait_for_marker(path: &std::path::Path, what: &str) {
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_secs(20))
            .expect("a deadline inside the clock");
        while !path.exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{what} marker ({}) never came up after waiting 20 s",
                path.display()
            );
            // Poll interval: checks marker file existence every 50 ms.
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Wait for a window to be hidden by the helper and confirmed off-screen by CoreGraphics.
    async fn wait_for_window_off_screen(
        markers: &std::path::Path,
        target: WindowId,
        within: Duration,
    ) {
        let deadline =
            tokio::time::Instant::now().checked_add(within).expect("a deadline inside the clock");
        loop {
            let helper_hidden = std::fs::read_to_string(markers.join("state"))
                .is_ok_and(|s| s.starts_with("hide visible=false"));
            if helper_hidden && !slopty_capture::window_on_screen(target) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "window {target:?} did not hide and leave screen after waiting {within:?}"
            );
            // Poll interval: checks helper state and CoreGraphics on-screen status every 10 ms.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Wait for the worker to report `want` about a stream's capture source.
    async fn wait_for_source(
        events: &mut tokio::sync::mpsc::Receiver<LinkEvent>,
        want: SourceState,
        within: Duration,
    ) -> bool {
        let deadline =
            tokio::time::Instant::now().checked_add(within).expect("a deadline inside the clock");
        while tokio::time::Instant::now() < deadline {
            let Ok(Some(event)) =
                tokio::time::timeout(Duration::from_millis(500), events.recv()).await
            else {
                continue;
            };
            if let LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Source { state, .. })) = event
                && state == want
            {
                return true;
            }
        }
        false
    }

    /// The named pasteboard a test's the worker syncs instead of the general one.
    fn pasteboard_name(dir: &std::path::Path) -> String {
        let leaf = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        format!("dev.aislopware.slopty.e2e.{leaf}")
    }

    /// A test's pasteboard, released however the test ends.
    struct TestBoard(slopty_input::MacBoard);

    impl Drop for TestBoard {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    /// The first control message `pick` takes within a step, skipping the others.
    async fn next_msg<T>(
        worker: &mut WorkerConn,
        mut pick: impl FnMut(WorkerMsg) -> Option<T>,
    ) -> T {
        let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
        loop {
            let msg = tokio::time::timeout_at(deadline, worker.rx.recv()).await.unwrap().unwrap();
            if let Some(found) = pick(msg) {
                return found;
            }
        }
    }

    /// Whether a message `pick` takes arrives within `wait`.
    async fn arrives<T>(
        worker: &mut WorkerConn,
        wait: Duration,
        mut pick: impl FnMut(WorkerMsg) -> Option<T>,
    ) -> bool {
        let deadline = tokio::time::Instant::now().checked_add(wait).unwrap();
        loop {
            match tokio::time::timeout_at(deadline, worker.rx.recv()).await {
                Err(_elapsed) => return false,
                Ok(msg) => {
                    if pick(msg.unwrap()).is_some() {
                        return true;
                    }
                }
            }
        }
    }

    /// Every message sent before this has been handled by the worker (it answers in order).
    async fn settled(worker: &mut WorkerConn) {
        let sent_at = slopty_core::MonoTime::now();
        worker.tx.send(&ClientMsg::Ping { sent_at }).await.unwrap();
        next_msg(worker, |m| {
            matches!(m, WorkerMsg::Pong { sent_at: s } if s == sent_at).then_some(())
        })
        .await;
    }

    /// Everything left on a raw stream.
    async fn drain(rx: &mut streams::RawRecv) -> Vec<u8> {
        let mut got = Vec::new();
        while let Some(chunk) = rx.chunk(1 << 20).await.unwrap() {
            got.extend_from_slice(&chunk);
        }
        got
    }

    /// Open `/bin/sh` in `cwd`, not attached.
    async fn open_shell(worker: &mut WorkerConn, cwd: &std::path::Path) -> SessionId {
        worker
            .tx
            .send(&ClientMsg::OpenSession(OpenSession {
                size: TermSize { cols: 80, rows: 24, ..TermSize::default() },
                cwd: Some(cwd.to_string_lossy().into_owned()),
                command: vec!["/bin/sh".to_owned()],
                env: vec![("PS1".to_owned(), "$ ".to_owned())],
                title: None,
                attach: false,
            }))
            .await
            .unwrap();
        next_msg(worker, |m| match m {
            WorkerMsg::SessionOpened(summary) => Some(summary.id),
            _ => None,
        })
        .await
    }

    fn digest(bytes: &[u8]) -> [u8; 32] {
        blake3::hash(bytes).into()
    }

    /// Worker → client: a change is announced while watched, small text inline and a picture
    /// fetched over a bulk stream. Client → worker: the offer waits for the paste chord, the
    /// picture is fetched from the client, and what lands is not announced back. Unwatched, a
    /// change is not announced. All on a named pasteboard, never the user's.
    #[tokio::test]
    async fn the_clipboard_is_announced_fetched_and_pasted_both_ways() {
        use slopty_input::pasteboard::{Board as _, ORIGIN_TYPE, Rep};
        use slopty_proto::input::{KeyAction, KeyCode, Mods};
        use slopty_proto::screen::ScreenInput;
        use slopty_proto::transfer::{ClipItem, ClipMsg, Offer, Peer, Purpose};

        let dir = tempfile::tempdir().unwrap();
        let board = TestBoard(slopty_input::MacBoard::named(&pasteboard_name(dir.path())));
        let (_guard, mut worker) = connect(dir.path()).await;
        let (text, png) = (Rep::Text.uti(), Rep::Png.uti());
        let offered = |m| match m {
            WorkerMsg::Clip(ClipMsg::Offer(offer)) => Some(offer),
            _ => None,
        };

        worker.tx.send(&ClientMsg::Clip(ClipMsg::Watch(true))).await.unwrap();
        settled(&mut worker).await;
        let picture: Vec<u8> = (0..200_000_u32).map(|i| (i % 253) as u8).collect();
        let copied =
            vec![(png.clone(), picture.clone()), (text.clone(), b"copied on the worker".to_vec())];
        board.0.write(&[copied]).unwrap();
        let offer = next_msg(&mut worker, offered).await;
        assert_eq!(offer.origin, Peer::Worker(worker.ack.worker));
        let utis: Vec<&str> = offer.items.iter().map(|i| i.uti.as_str()).collect();
        assert_eq!(utis, [png.as_str(), text.as_str()], "richest first");
        assert_eq!(offer.items[0].inline, None, "a picture is listed, not pushed");
        assert_eq!(
            (offer.items[0].size, offer.items[0].hash),
            (picture.len() as u64, digest(&picture))
        );
        assert_eq!(offer.items[1].inline.as_deref(), Some(&b"copied on the worker"[..]));

        let generation = offer.generation;
        let fetch = ClipMsg::Fetch { generation, uti: png.clone() };
        worker.tx.send(&ClientMsg::Clip(fetch)).await.unwrap();
        let Uni::Bulk { header, mut rx } =
            tokio::time::timeout(STEP, streams::accept_uni(&worker.conn)).await.unwrap().unwrap()
        else {
            panic!("the picture comes as a bulk stream");
        };
        assert_eq!(header.purpose, Purpose::Clip { generation, uti: png.clone() });
        assert!(drain(&mut rx).await == picture, "the picture arrives whole");
        worker
            .tx
            .send(&ClientMsg::Clip(ClipMsg::Fetch { generation, uti: text.clone() }))
            .await
            .unwrap();
        let data = next_msg(&mut worker, |m| match m {
            WorkerMsg::Clip(ClipMsg::Data { bytes, .. }) => Some(bytes),
            _ => None,
        })
        .await;
        assert_eq!(data, b"copied on the worker");

        // The client copies a picture and some text.
        let theirs: Vec<u8> = (0..100_000_u32).map(|i| (i % 7) as u8).collect();
        let offer = Offer {
            origin: Peer::Client(ClientId::new()),
            generation: 1,
            items: vec![
                ClipItem {
                    uti: png.clone(),
                    size: theirs.len() as u64,
                    hash: digest(&theirs),
                    inline: None,
                },
                ClipItem {
                    uti: text.clone(),
                    size: 15,
                    hash: digest(b"from the client"),
                    inline: Some(b"from the client".to_vec()),
                },
            ],
        };
        worker.tx.send(&ClientMsg::Clip(ClipMsg::Offer(offer))).await.unwrap();
        settled(&mut worker).await;
        assert_eq!(board.0.data(&text).unwrap(), b"copied on the worker", "announced, not pushed");
        // ⌘V aimed at a stream that does not exist: no event is posted anywhere.
        let chord = ScreenInput::Key {
            code: KeyCode::V,
            action: KeyAction::Press,
            mods: Mods::SUPER,
            text: None,
        };
        let input = ScreenRequest::Input { stream: slopty_core::StreamId(77), input: chord };
        worker.tx.send(&ClientMsg::Screen(input)).await.unwrap();
        let (generation, uti) = next_msg(&mut worker, |m| match m {
            WorkerMsg::Clip(ClipMsg::Fetch { generation, uti }) => Some((generation, uti)),
            _ => None,
        })
        .await;
        assert_eq!((generation, uti.as_str()), (1, png.as_str()), "only what was not inline");
        let data = ClipMsg::Data { generation, uti, bytes: theirs.clone() };
        worker.tx.send(&ClientMsg::Clip(data)).await.unwrap();
        let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
        while board.0.data(&png).as_deref() != Some(theirs.as_slice()) {
            assert!(tokio::time::Instant::now() < deadline, "the picture reaches the pasteboard");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(board.0.data(&text).unwrap(), b"from the client");
        assert!(board.0.types().iter().any(|t| t == ORIGIN_TYPE), "stamped with its origin");
        assert!(
            !arrives(&mut worker, Duration::from_millis(800), offered).await,
            "not announced back"
        );

        worker.tx.send(&ClientMsg::Clip(ClipMsg::Watch(false))).await.unwrap();
        settled(&mut worker).await;
        board.0.write(&[vec![(text.clone(), b"nobody watches".to_vec())]]).unwrap();
        assert!(!arrives(&mut worker, Duration::from_millis(800), offered).await, "unwatched");
    }

    /// Two files dropped on a terminal land in its shell's directory; one is cut halfway,
    /// resumed from what the worker kept, and lands whole. A name already taken lands in the
    /// drop directory. The paths to paste come back once every file is in.
    #[tokio::test]
    async fn an_upload_lands_in_the_shells_directory_and_resumes_after_a_cut() {
        use slopty_core::XferId;
        use slopty_proto::transfer::{BulkHeader, Dest, Purpose, XferMsg};

        let dir = tempfile::tempdir().unwrap();
        let (_guard, mut worker) = connect(dir.path()).await;
        let cwd = dir.path().join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("taken.txt"), b"mine").unwrap();
        let cwd = cwd.canonicalize().unwrap();
        let session = open_shell(&mut worker, &cwd).await;
        let header = |xfer, name: &str, size: usize, offset: u64| BulkHeader {
            xfer,
            purpose: Purpose::Upload,
            name: name.to_owned(),
            size: size as u64,
            mtime_ms: 1_700_000_000_000,
            mode: 0o640,
            offset,
        };
        let done = |m| match m {
            WorkerMsg::Xfer(XferMsg::Done { name, path, hash, .. }) => Some((name, path, hash)),
            WorkerMsg::Xfer(XferMsg::Failed { name, error, .. }) => {
                panic!("{name:?} failed: {error}")
            }
            _ => None,
        };

        let xfer = XferId::new();
        let small = b"a small file".to_vec();
        let big: Vec<u8> = (0..3_000_000_u32).map(|i| (i % 249) as u8).collect();
        let begin = XferMsg::Begin {
            xfer,
            dest: Some(Dest::SessionCwd(session)),
            files: 2,
            bytes: (small.len() + big.len()) as u64,
        };
        worker.tx.send(&ClientMsg::Xfer(begin)).await.unwrap();
        let mut send =
            streams::open_bulk(&worker.conn, header(xfer, "a.txt", small.len(), 0)).await.unwrap();
        send.write_all(&small).await.unwrap();
        send.finish().unwrap();
        let (name, path, hash) = next_msg(&mut worker, done).await;
        assert_eq!((name.as_str(), hash), ("a.txt", digest(&small)));
        assert_eq!(PathBuf::from(&path), cwd.join("a.txt"));

        // Half the big file, then the stream is cut.
        let mut send =
            streams::open_bulk(&worker.conn, header(xfer, "big.bin", big.len(), 0)).await.unwrap();
        send.write_all(&big[..1_500_000]).await.unwrap();
        let partial = cwd.join("big.bin.partial");
        let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
        while std::fs::metadata(&partial).map_or(0, |m| m.len()) == 0 {
            assert!(tokio::time::Instant::now() < deadline, "the worker writes as bytes arrive");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        send.reset(0_u32.into()).unwrap();
        let failed = next_msg(&mut worker, |m| match m {
            WorkerMsg::Xfer(XferMsg::Failed { name, .. }) => Some(name),
            _ => None,
        })
        .await;
        assert_eq!(failed.as_deref(), Some("big.bin"));
        assert!(!cwd.join("big.bin").exists(), "half a file is never under its name");
        worker
            .tx
            .send(&ClientMsg::Xfer(XferMsg::Resume { xfer, name: "big.bin".to_owned() }))
            .await
            .unwrap();
        let durable = next_msg(&mut worker, |m| match m {
            WorkerMsg::Xfer(XferMsg::Offset { durable, .. }) => Some(durable),
            _ => None,
        })
        .await;
        assert!(durable > 0 && durable <= 1_500_000, "kept what arrived: {durable}");
        let rest = &big[usize::try_from(durable).unwrap()..];
        let mut send =
            streams::open_bulk(&worker.conn, header(xfer, "big.bin", big.len(), durable))
                .await
                .unwrap();
        send.write_all(rest).await.unwrap();
        send.finish().unwrap();
        let (name, _path, hash) = next_msg(&mut worker, done).await;
        assert_eq!((name.as_str(), hash), ("big.bin", digest(&big)), "whole across the cut");
        let paths = next_msg(&mut worker, |m| match m {
            WorkerMsg::Xfer(XferMsg::Finished { xfer: x, paths }) if x == xfer => Some(paths),
            _ => None,
        })
        .await;
        let expected: Vec<String> =
            ["a.txt", "big.bin"].map(|n| cwd.join(n).to_string_lossy().into_owned()).into();
        assert_eq!(paths, expected);
        assert_eq!(digest(&std::fs::read(cwd.join("big.bin")).unwrap()), digest(&big));

        // A name the directory already has lands in the drop directory instead.
        let clash = XferId::new();
        let begin = XferMsg::Begin {
            xfer: clash,
            dest: Some(Dest::SessionCwd(session)),
            files: 1,
            bytes: 5,
        };
        worker.tx.send(&ClientMsg::Xfer(begin)).await.unwrap();
        let mut send =
            streams::open_bulk(&worker.conn, header(clash, "taken.txt", 5, 0)).await.unwrap();
        send.write_all(b"yours").await.unwrap();
        send.finish().unwrap();
        let paths = next_msg(&mut worker, |m| match m {
            WorkerMsg::Xfer(XferMsg::Finished { xfer: x, paths }) if x == clash => Some(paths),
            _ => None,
        })
        .await;
        let dropped = dir.path().join("drop").join(clash.to_string()).join("taken.txt");
        assert_eq!(paths, [dropped.to_string_lossy().into_owned()]);
        assert_eq!(std::fs::read(cwd.join("taken.txt")).unwrap(), b"mine", "untouched");
        assert_eq!(std::fs::read(dropped).unwrap(), b"yours");
        let close = ClientMsg::Term { session, req: TermRequest::Close };
        worker.tx.send(&close).await.unwrap();
    }

    /// A client's tunnel reaches a TCP server on the worker's loopback, both ways, and a
    /// half-close on one side ends the other.
    #[tokio::test]
    async fn a_tunnel_reaches_a_local_echo_server() {
        use tokio::io::AsyncWriteExt as _;
        let dir = tempfile::tempdir().unwrap();
        let (_guard, worker) = connect(dir.path()).await;
        let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = echo.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _peer) = echo.accept().await.unwrap();
            let (mut rd, mut wr) = socket.split();
            tokio::io::copy(&mut rd, &mut wr).await.unwrap();
            wr.shutdown().await.unwrap();
        });
        let (mut send, mut rx) = streams::open_tunnel(&worker.conn, port).await.unwrap();
        let request: Vec<u8> = (0..300_000_u32).map(|i| (i % 241) as u8).collect();
        send.write_all(&request).await.unwrap();
        send.finish().unwrap();
        let echoed = tokio::time::timeout(STEP, drain(&mut rx)).await.unwrap();
        assert!(echoed == request, "the bytes come back whole and in order");
        tokio::time::timeout(STEP, server).await.unwrap().unwrap();
    }

    /// `nc -l` typed into a real shell is announced as its session's port, and the set is
    /// announced empty once it stops.
    #[tokio::test]
    async fn ports_follow_a_listener_in_a_real_shell() {
        let dir = tempfile::tempdir().unwrap();
        let (_guard, mut worker) = connect(dir.path()).await;
        let session = open_shell(&mut worker, dir.path()).await;
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let listen = format!("nc -l 127.0.0.1 {port}\n").into_bytes();
        worker.tx.send(&ClientMsg::Term { session, req: TermRequest::Raw(listen) }).await.unwrap();
        let ports = next_msg(&mut worker, |m| match m {
            WorkerMsg::Ports { session: s, ports } if s == session && !ports.is_empty() => {
                Some(ports)
            }
            _ => None,
        })
        .await;
        assert_eq!(ports.len(), 1, "{ports:?}");
        assert_eq!((ports[0].number, ports[0].process.as_str()), (port, "nc"));
        assert_eq!(ports[0].session, Some(session));
        worker
            .tx
            .send(&ClientMsg::Term { session, req: TermRequest::Raw(b"\x03".to_vec()) })
            .await
            .unwrap();
        next_msg(&mut worker, |m| match m {
            WorkerMsg::Ports { session: s, ports } if s == session && ports.is_empty() => Some(()),
            _ => None,
        })
        .await;
        let close = ClientMsg::Term { session, req: TermRequest::Close };
        worker.tx.send(&close).await.unwrap();
    }
}
