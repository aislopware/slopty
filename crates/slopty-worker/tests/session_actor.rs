//! The session actor against a real shell on an in-process PTY (no ptyd).

#[cfg(test)]
mod actor {
    use std::time::Duration;

    use slopty_client::term::{Effect, TermState};
    use slopty_core::{ClientId, SessionId};
    use slopty_grid::LineIndex;
    use slopty_proto::input::{CellMetrics, KeyAction, KeyCode, KeyEvent, Mods};
    use slopty_proto::terminal::{TermColors, TermEvent, TermRequest, TermSize};
    use slopty_pty::{Pty, SpawnSpec};
    use slopty_worker::session::{self, Outbound, SessionStart, Tap};
    use tokio::sync::mpsc;

    fn size(cols: u16, rows: u16) -> TermSize {
        TermSize { cols, rows, metrics: CellMetrics { cell_width: 8, cell_height: 16 } }
    }

    fn start(command: &[&str]) -> (session::SessionHandle, tokio::process::Child) {
        let (handle, child, _tap) = start_tapped(command, Vec::new());
        (handle, child)
    }

    fn start_tapped(
        command: &[&str],
        checkpoint: Vec<u8>,
    ) -> (session::SessionHandle, tokio::process::Child, mpsc::Receiver<Tap>) {
        let (tap, tap_rx) = mpsc::channel(64);
        let pty = Pty::open(size(40, 6)).unwrap();
        let child = pty
            .spawn(&SpawnSpec {
                command: command.iter().map(|s| (*s).to_owned()).collect(),
                cwd: None,
                env: vec![("PS1".to_owned(), "$ ".to_owned())],
                size: size(40, 6),
            })
            .unwrap();
        let handle = session::spawn(SessionStart {
            id: SessionId::new(),
            master: pty.into_master(),
            checkpoint,
            backlog: Vec::new(),
            tap,
            size: size(40, 6),
            scrollback_lines: 1000,
            exited: None,
            port_hints: None,
        })
        .unwrap();
        (handle, child, tap_rx)
    }

    /// The event an actor sent, decoded from the wire as the client would.
    fn event(out: &Outbound) -> TermEvent {
        let mut buf = bytes::BytesMut::from(out.wire());
        slopty_proto::codec::try_decode(&mut buf).unwrap().expect("one whole event")
    }

    /// A client's view of a session: its sink, and the state its events build, as the app
    /// applies them.
    struct Viewer {
        rx: mpsc::Receiver<Outbound>,
        state: TermState,
    }

    /// A sink `depth` events deep and the viewer reading it.
    fn viewer(depth: usize) -> (mpsc::Sender<Outbound>, Viewer) {
        let (tx, rx) = mpsc::channel(depth);
        (tx, Viewer { rx, state: TermState::new(size(40, 6)) })
    }

    /// Apply events to the viewer's state until `pred` holds of them and its screen.
    async fn wait_for(
        viewer: &mut Viewer,
        mut pred: impl FnMut(&[TermEvent], &slopty_grid::Screen) -> bool,
    ) -> (Vec<TermEvent>, slopty_grid::Screen) {
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        loop {
            let ev = tokio::time::timeout_at(deadline, viewer.rx.recv())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "timeout waiting for events; {seen:?}\nscreen:\n{}",
                        text(viewer.state.screen())
                    )
                })
                .expect("sink closed");
            let ev = event(&ev);
            let _effects = viewer.state.apply(ev.clone());
            seen.push(ev);
            if pred(&seen, viewer.state.screen()) {
                return (seen, viewer.state.screen().clone());
            }
        }
    }

    fn text(screen: &slopty_grid::Screen) -> String {
        screen.lines().iter().map(|l| l.text()).collect::<Vec<_>>().join("\n")
    }

    fn key(seq: u64, code: KeyCode, text: &str) -> TermRequest {
        TermRequest::Key(KeyEvent {
            seq,
            action: KeyAction::Press,
            code,
            mods: Mods::empty(),
            consumed_mods: Mods::empty(),
            text: Some(text.to_owned()),
            unshifted: text.chars().next(),
            composing: false,
            option_as_alt: false,
        })
    }

    #[tokio::test]
    async fn attach_type_and_see_echo_with_input_ack() {
        let (session, mut child) = start(&["/bin/sh", "-c", "cat"]);
        let me = ClientId::new();
        let (tx, mut rx) = viewer(64);
        session.attach(me, size(40, 6), tx).unwrap();
        let (events, _) = wait_for(&mut rx, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Frame(f) if f.full))
        })
        .await;
        assert!(
            events.iter().any(|e| matches!(e, TermEvent::Driver { you: true })),
            "first viewer drives"
        );

        session.request(me, key(1, KeyCode::H, "h")).unwrap();
        session.request(me, key(2, KeyCode::I, "i")).unwrap();
        session.request(me, key(3, KeyCode::Enter, "\r")).unwrap();
        let (events, screen) = wait_for(&mut rx, |_, s| text(s).contains("hi\nhi")).await;
        assert!(text(&screen).starts_with("hi\nhi"), "tty echo then cat echo: {:?}", text(&screen));
        let last_ack = events
            .iter()
            .filter_map(|e| if let TermEvent::Frame(f) = e { Some(f.input_ack) } else { None })
            .max();
        assert_eq!(last_ack, Some(3), "the frame carrying the echo acknowledges the keys");

        session.request(me, TermRequest::Raw(b"\x04".to_vec())).unwrap();
        // ptyd reaps the child and the worker passes the status on; here the test is both.
        let status = child.wait().await.unwrap();
        session.exited(status.code().unwrap());
        let (events, _) =
            wait_for(&mut rx, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Exited { .. })))
                .await;
        assert!(events.iter().any(|e| matches!(e, TermEvent::Exited { status: 0 })));
        session.close();
    }

    /// Orchestration resizes a terminal only while no client shows it, checked and applied in
    /// one step, so a client that attached first keeps the size its window set.
    #[tokio::test]
    async fn a_resize_applies_only_while_no_client_shows_the_terminal() {
        let (session, _child) = start(&["/bin/sh", "-c", "cat"]);
        assert_eq!(session.resize_unviewed(size(100, 30)).await.unwrap(), 0);
        let snap = session.snapshot().await.unwrap();
        assert_eq!((snap.size.cols, snap.size.rows), (100, 30), "nobody watches: applied");

        let me = ClientId::new();
        let (tx, mut rx) = viewer(64);
        session.attach(me, size(40, 6), tx).unwrap();
        wait_for(&mut rx, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Frame(f) if f.full)))
            .await;
        assert_eq!(session.resize_unviewed(size(150, 40)).await.unwrap(), 1);
        let snap = session.snapshot().await.unwrap();
        assert_eq!((snap.size.cols, snap.size.rows), (40, 6), "the viewer's size stands");
        session.close();
    }

    /// Every read goes to the tap as it is, and once the shell is quiet a checkpoint follows
    /// that a fresh actor can start from: the second actor shows the first one's screen.
    #[tokio::test]
    async fn output_is_tapped_and_a_quiet_session_checkpoints() {
        let (session, mut child, mut taps) = start_tapped(&["/bin/sh", "-c", "cat"], Vec::new());
        let me = ClientId::new();
        let (tx, mut rx) = viewer(64);
        session.attach(me, size(40, 6), tx).unwrap();
        session.request(me, TermRequest::Raw(b"tapped-line\r".to_vec())).unwrap();
        let _seen = wait_for(&mut rx, |_, s| text(s).contains("tapped-line\ntapped-line")).await;

        let deadline = tokio::time::Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        let mut output = Vec::new();
        let checkpoint = loop {
            match tokio::time::timeout_at(deadline, taps.recv()).await.unwrap().unwrap() {
                Tap::Output { bytes, .. } => output.extend_from_slice(&bytes),
                Tap::Checkpoint { state, .. }
                    if output.windows(11).any(|w| w == b"tapped-line") =>
                {
                    break state;
                }
                Tap::Checkpoint { .. } | Tap::Resize { .. } => {}
            }
        };
        let text_out = String::from_utf8_lossy(&output);
        assert!(text_out.contains("tapped-line\r\ntapped-line"), "tapped: {text_out:?}");

        let (next, mut cat2, mut taps2) = start_tapped(&["/bin/sh", "-c", "cat"], checkpoint);
        let (tx2, mut rx2) = viewer(64);
        next.attach(ClientId::new(), size(40, 6), tx2).unwrap();
        let (_, screen) = wait_for(&mut rx2, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Frame(f) if f.full))
        })
        .await;
        assert!(
            text(&screen).starts_with("tapped-line\ntapped-line"),
            "replayed: {:?}",
            text(&screen)
        );
        // The replayed state is checkpointed again by the new actor without any output.
        let again = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Tap::Checkpoint { state, .. } = taps2.recv().await.unwrap() {
                    break state;
                }
            }
        })
        .await
        .unwrap();
        assert!(!again.is_empty());

        session.close();
        next.close();
        let _first = child.kill().await;
        let _second = cat2.kill().await;
    }

    #[tokio::test]
    async fn scrollback_fetch_and_driver_handoff() {
        let (session, mut child) = start(&[
            "/bin/sh",
            "-c",
            "i=0; while [ $i -lt 20 ]; do echo line$i; i=$((i+1)); done; read x",
        ]);
        let a = ClientId::new();
        let b = ClientId::new();
        let (tx_a, mut rx_a) = viewer(64);
        let (tx_b, mut rx_b) = viewer(64);
        session.attach(a, size(40, 6), tx_a).unwrap();
        let (events, _) = wait_for(&mut rx_a, |_, s| text(s).contains("line19")).await;

        // Frames say where the screen sits in the absolute numbering; fetch the lines above it.
        let frame = events
            .iter()
            .rev()
            .find_map(|e| if let TermEvent::Frame(f) = e { Some(f.clone()) } else { None })
            .unwrap();
        assert!(
            frame.first_visible_line.0 >= 14,
            "20 lines through 6 rows leave history: {frame:?}"
        );
        session.request(a, TermRequest::FetchLines { start: LineIndex(0), count: 5 }).unwrap();
        let (events, _) =
            wait_for(&mut rx_a, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Lines { .. })))
                .await;
        let lines = events
            .iter()
            .find_map(|e| {
                if let TermEvent::Lines { start, lines } = e {
                    Some((*start, lines.clone()))
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(lines.0, LineIndex(0));
        assert_eq!(
            lines.1.iter().map(slopty_grid::Line::text).collect::<Vec<_>>(),
            ["line0", "line1", "line2", "line3", "line4"]
        );

        // Second viewer with a different size: does not drive until the first leaves.
        session.attach(b, size(60, 10), tx_b).unwrap();
        let (events, _) = wait_for(&mut rx_b, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Frame(f) if f.full))
        })
        .await;
        assert!(!events.iter().any(|e| matches!(e, TermEvent::Driver { you: true })));
        assert!(
            events.iter().any(|e| matches!(e, TermEvent::Frame(f) if f.cols == 40 && f.rows == 6))
        );
        session.detach(a).unwrap();
        let (events, _) = wait_for(&mut rx_b, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Resized { cols: 60, rows: 10 }))
        })
        .await;
        assert!(events.iter().any(|e| matches!(e, TermEvent::Driver { you: true })));

        session.request(b, TermRequest::Raw(b"\r".to_vec())).unwrap();
        child.wait().await.unwrap();
        session.close();
    }

    /// A viewer that claims the wheel gets it, the old driver is told, the PTY takes the new
    /// driver's size and every later resize of theirs; a search finds a line and rejects a
    /// bad pattern; a probe names the foreground process; releasing the wheel is told too.
    #[tokio::test]
    async fn a_claimed_wheel_resizes_and_a_search_and_probe_answer() {
        let (session, mut child) =
            start(&["/bin/sh", "-c", "printf 'alpha\\nbeta\\ngamma\\n'; read x; exit 0"]);
        let a = ClientId::new();
        let b = ClientId::new();
        let (tx_a, mut rx_a) = viewer(64);
        let (tx_b, mut rx_b) = viewer(64);
        session.attach(a, size(40, 6), tx_a).unwrap();
        wait_for(&mut rx_a, |_, s| text(s).contains("gamma")).await;
        session.attach(b, size(60, 10), tx_b).unwrap();
        wait_for(&mut rx_b, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Frame(f) if f.full)))
            .await;

        // b claims the wheel: a is told, b is told, and the PTY becomes b's size.
        session.request(b, TermRequest::Drive { drive: true }).unwrap();
        wait_for(&mut rx_a, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Driver { you: false }))
        })
        .await;
        let (events, _) = wait_for(&mut rx_b, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Resized { cols: 60, rows: 10 }))
        })
        .await;
        assert!(events.iter().any(|e| matches!(e, TermEvent::Driver { you: true })));

        // The driver's resize moves the PTY; a viewer's does not.
        session.request(a, TermRequest::Resize(size(20, 4))).unwrap();
        session.request(b, TermRequest::Resize(size(50, 8))).unwrap();
        let (events, _) = wait_for(&mut rx_a, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Resized { cols: 50, rows: 8 }))
        })
        .await;
        assert!(
            !events.iter().any(|e| matches!(e, TermEvent::Resized { cols: 20, rows: 4 })),
            "a viewer's size is recorded, never applied: {events:?}"
        );

        // A search over the screen and history; a bad pattern is refused, not fatal.
        session
            .request(a, TermRequest::Search { needle: "beta".to_owned(), max: 10, regex: false })
            .unwrap();
        let (events, _) =
            wait_for(&mut rx_a, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Matches { .. })))
                .await;
        assert!(events.iter().any(|e| matches!(
            e,
            TermEvent::Matches { needle, total: 1, matches } if needle == "beta" && matches.len() == 1
        )));
        session
            .request(a, TermRequest::Search { needle: "(".to_owned(), max: 10, regex: true })
            .unwrap();
        wait_for(&mut rx_a, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::SearchInvalid { needle, .. } if needle == "("))
        })
        .await;

        // The probe names the shell waiting on `read` (macOS's `/bin/sh` is a bash), with
        // no title or cwd announced.
        let probe = session.probe().await.unwrap();
        assert_eq!(
            probe.foreground.as_ref().and_then(|f| f.argv.first()).map(String::as_str),
            Some("/bin/sh"),
            "{probe:?}"
        );
        assert_eq!((probe.title, probe.cwd), (None, None));

        // Focus reaches the engine (nothing is encoded while the program has not asked).
        session.request(b, TermRequest::Focus { focused: true }).unwrap();
        // Releasing the wheel is told to the one who held it.
        session.request(b, TermRequest::Drive { drive: false }).unwrap();
        wait_for(&mut rx_b, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Driver { you: false }))
        })
        .await;

        session.request(b, TermRequest::Raw(b"\r".to_vec())).unwrap();
        child.wait().await.unwrap();
        session.close();
    }

    /// A viewer's colours are kept, never applied; the driver's answer the program's colour
    /// query. The shell (raw input: the reply has no newline for a cooked read to wait on)
    /// asks OSC 11 `?` each time it is nudged and prints the reply through `cat -v` (the
    /// ESCs as `^[`): 25 bytes, the `rgb:rrrr/gggg/bbbb` answer up to its terminator,
    /// which the next nudge swallows.
    #[tokio::test]
    async fn the_drivers_colours_answer_a_colour_query() {
        let (session, mut child) = start(&[
            "/bin/sh",
            "-c",
            "stty -icanon -echo; for i in 1 2; do read x; printf '\\033]11;?\\033\\\\'; dd bs=1 count=25 2>/dev/null | cat -v; echo; done; exit 0",
        ]);
        let a = ClientId::new();
        let b = ClientId::new();
        let (tx_a, mut rx_a) = viewer(64);
        let (tx_b, mut rx_b) = viewer(64);
        session.attach(a, size(60, 6), tx_a).unwrap();
        wait_for(&mut rx_a, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Driver { you: true }))
        })
        .await;
        session.attach(b, size(60, 6), tx_b).unwrap();
        wait_for(&mut rx_b, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Frame(f) if f.full)))
            .await;
        let white = TermColors { fg: [0; 3], bg: [0xff; 3], cursor: [0; 3], ansi: [[0; 3]; 16] };
        session.request(b, TermRequest::Colors(white)).unwrap();
        // b only views: the answer is still the default dark background.
        session.request(a, TermRequest::Raw(b"\r".to_vec())).unwrap();
        wait_for(&mut rx_a, |_, s| text(s).contains("rgb:0e0e/0f0f/1212")).await;
        // b drives: its white answers.
        session.request(b, TermRequest::Drive { drive: true }).unwrap();
        wait_for(&mut rx_b, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Driver { you: true }))
        })
        .await;
        session.request(a, TermRequest::Raw(b"\r".to_vec())).unwrap();
        wait_for(&mut rx_a, |_, s| text(s).contains("rgb:ffff/ffff/ffff")).await;
        child.wait().await.unwrap();
        session.close();
    }

    /// A program's colour change reaches every viewer, and a client attaching afterwards
    /// learns the whole set ahead of its first frame.
    #[tokio::test]
    async fn the_programs_colours_reach_every_viewer_and_a_late_attach() {
        let (session, mut child) = start(&[
            "/bin/sh",
            "-c",
            "printf '\\033]11;#282c34\\033\\\\'; read x; printf '\\033]111\\033\\\\'; exit 0",
        ]);
        let a = ClientId::new();
        let b = ClientId::new();
        let (tx_a, mut rx_a) = viewer(64);
        let (tx_b, mut rx_b) = viewer(64);
        session.attach(a, size(60, 6), tx_a).unwrap();
        let bg = Some([0x28, 0x2c, 0x34]);
        let (events, _) = wait_for(&mut rx_a, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Colors(c) if c.bg == bg))
        })
        .await;
        assert!(
            events.iter().all(|e| !matches!(e, TermEvent::Colors(c) if c.bg.is_none())),
            "only the change is sent, not the default set: {events:?}"
        );
        session.attach(b, size(60, 6), tx_b).unwrap();
        let (events, _) = wait_for(&mut rx_b, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Frame(f) if f.full))
        })
        .await;
        let colors = events.iter().position(|e| matches!(e, TermEvent::Colors(c) if c.bg == bg));
        let frame = events.iter().position(|e| matches!(e, TermEvent::Frame(_)));
        assert!(colors < frame && colors.is_some(), "colours before the frame: {events:?}");
        session.request(a, TermRequest::Raw(b"\r".to_vec())).unwrap();
        wait_for(&mut rx_b, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Colors(c) if c.bg.is_none()))
        })
        .await;
        child.wait().await.unwrap();
        session.close();
    }

    /// A restored session (a checkpoint from the last worker) tells its first attach the
    /// program's colours and the directory the replay carried, ahead of the frame.
    #[tokio::test]
    async fn a_restored_session_tells_the_first_attach_what_the_replay_said() {
        let mut e = slopty_engine::GhosttyEngine::new(slopty_engine::EngineConfig {
            size: size(40, 6),
            scrollback_lines: 100,
        })
        .unwrap();
        e.write(b"\x1b]7;file:///tmp\x1b\\\x1b]11;#282c34\x1b\\hello");
        let mut checkpoint = Vec::new();
        e.checkpoint(&mut checkpoint).unwrap();
        let (session, mut child, _tap) =
            start_tapped(&["/bin/sh", "-c", "read x; exit 0"], checkpoint);
        let a = ClientId::new();
        let (tx_a, mut rx_a) = viewer(64);
        session.attach(a, size(40, 6), tx_a).unwrap();
        let (events, screen) = wait_for(&mut rx_a, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Frame(f) if f.full))
        })
        .await;
        let frame = events.iter().position(|e| matches!(e, TermEvent::Frame(_)));
        let colors = events
            .iter()
            .position(|e| matches!(e, TermEvent::Colors(c) if c.bg == Some([0x28, 0x2c, 0x34])));
        assert!(colors.is_some() && colors < frame, "{events:?}");
        assert!(
            events.iter().any(|e| matches!(e, TermEvent::Cwd { path, .. } if path == "/tmp")),
            "{events:?}"
        );
        assert!(text(&screen).contains("hello"), "{}", text(&screen));
        session.request(a, TermRequest::Raw(b"\r".to_vec())).unwrap();
        child.wait().await.unwrap();
        session.close();
    }

    /// The opener drives even when another client attaches first.
    #[tokio::test]
    async fn reserved_driver_beats_the_first_attach() {
        let (session, mut child) = start(&["/bin/sh", "-c", "read x; exit 0"]);
        let opener = ClientId::new();
        let other = ClientId::new();
        session.reserve_driver(opener).unwrap();
        let (tx_other, mut rx_other) = viewer(64);
        let (tx_opener, mut rx_opener) = viewer(64);
        session.attach(other, size(60, 10), tx_other).unwrap();
        let (events, _) =
            wait_for(&mut rx_other, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Frame(_))))
                .await;
        assert!(!events.iter().any(|e| matches!(e, TermEvent::Driver { you: true })));
        assert!(events.iter().any(|e| matches!(e, TermEvent::Frame(f) if f.cols == 40)));

        session.attach(opener, size(50, 8), tx_opener).unwrap();
        let (events, _) = wait_for(&mut rx_opener, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Frame(f) if f.cols == 50 && f.rows == 8))
        })
        .await;
        assert!(events.iter().any(|e| matches!(e, TermEvent::Driver { you: true })));

        // Reserving the driver again is nothing to it, and a viewer's resize sizes nothing.
        session.reserve_driver(opener).unwrap();
        session.request(other, TermRequest::Resize(size(70, 12))).unwrap();
        session.request(opener, TermRequest::Raw(b"z".to_vec())).unwrap();
        let (events, _) = wait_for(&mut rx_opener, |_, s| text(s).contains('z')).await;
        assert!(
            !events.iter().any(|e| matches!(e, TermEvent::Driver { you: false })),
            "{events:?}"
        );
        let (events, _) = wait_for(&mut rx_other, |_, s| text(s).contains('z')).await;
        assert!(
            !events.iter().any(|e| matches!(e, TermEvent::Resized { cols: 70, .. })),
            "{events:?}"
        );

        session.request(opener, TermRequest::Raw(b"\r".to_vec())).unwrap();
        child.wait().await.unwrap();
        session.close();
    }

    /// A client that reconnects replaces its old viewer; the old connection dying afterwards
    /// must not evict the new one.
    #[tokio::test]
    async fn stale_connection_detach_keeps_the_reconnected_viewer() {
        let (session, mut child) = start(&["/bin/sh", "-c", "read x; exit 0"]);
        let a = ClientId::new();
        let (old_tx, mut old_rx) = viewer(64);
        let (new_tx, mut new_rx) = viewer(64);
        session.attach(a, size(40, 6), old_tx.clone()).unwrap();
        wait_for(&mut old_rx, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Frame(_)))).await;

        session.attach(a, size(40, 6), new_tx).unwrap();
        let (events, _) =
            wait_for(&mut new_rx, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Frame(_))))
                .await;
        // The reconnecting driver is told it still drives.
        assert!(events.iter().any(|e| matches!(e, TermEvent::Driver { you: true })));
        assert_eq!(session.snapshot().await.unwrap().viewers, 1);

        // The old connection goes away: scoped to its own sink, nothing changes.
        session.detach_sink(a, &old_tx).unwrap();
        assert_eq!(session.snapshot().await.unwrap().viewers, 1);
        // A live connection's own sink does detach its client.
        let b = ClientId::new();
        let (b_tx, mut b_rx) = viewer(64);
        session.attach(b, size(40, 6), b_tx.clone()).unwrap();
        wait_for(&mut b_rx, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Frame(_)))).await;
        assert_eq!(session.snapshot().await.unwrap().viewers, 2);
        session.detach_sink(b, &b_tx).unwrap();
        assert_eq!(session.snapshot().await.unwrap().viewers, 1);
        session.request(a, TermRequest::Raw(b"x".to_vec())).unwrap();
        let (_, screen) = wait_for(&mut new_rx, |_, screen| text(screen).contains('x')).await;
        assert!(text(&screen).contains('x'));

        // An unscoped detach still removes the client.
        session.detach(a).unwrap();
        assert_eq!(session.snapshot().await.unwrap().viewers, 0);
        session.request(a, TermRequest::Raw(b"\r".to_vec())).unwrap();
        child.wait().await.unwrap();
        session.close();
    }

    /// Closing the session drops the PTY master, so the child sees a hangup and exits.
    #[tokio::test]
    async fn closing_the_session_hangs_up_the_child() {
        let (session, mut child) = start(&["/bin/sh", "-c", "cat"]);
        let me = ClientId::new();
        let (tx, mut rx) = viewer(64);
        session.attach(me, size(40, 6), tx).unwrap();
        wait_for(&mut rx, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Frame(_)))).await;
        session.close();
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("the child exits once the master is closed")
            .unwrap();
        assert!(!status.success() || status.code() == Some(0), "{status:?}");
        assert!(matches!(session.snapshot().await, Err(slopty_worker::WorkerError::SessionClosed)));
    }

    /// A quiet spell inside an escape sequence does not checkpoint: the state would replace
    /// the sequence's head and its tail would print as text after a restart.
    #[tokio::test]
    async fn a_checkpoint_waits_for_the_end_of_an_escape_sequence() {
        let (session, mut child, mut taps) = start_tapped(
            &["/bin/sh", "-c", "printf '\\033]0;half'; sleep 2; printf 'done\\033\\\\'; sleep 30"],
            Vec::new(),
        );
        let (tx, _rx) = viewer(64);
        session.attach(ClientId::new(), size(40, 6), tx).unwrap();
        let deadline = tokio::time::Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        let mut output = Vec::new();
        let mut checkpoints_while_open = 0;
        loop {
            match tokio::time::timeout_at(deadline, taps.recv()).await.unwrap().unwrap() {
                Tap::Output { bytes, .. } => {
                    output.extend_from_slice(&bytes);
                    if output.windows(4).any(|w| w == b"done") {
                        break;
                    }
                }
                // The actor checkpoints once at start, before any output; only one
                // after the head of the sequence would be inside it.
                Tap::Checkpoint { .. } if !output.is_empty() => checkpoints_while_open += 1,
                Tap::Checkpoint { .. } | Tap::Resize { .. } => {}
            }
        }
        assert!(output.starts_with(b"\x1b]0;half"), "{output:?}");
        assert_eq!(checkpoints_while_open, 0, "no checkpoint inside the OSC");
        let state = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Tap::Checkpoint { state, .. } = taps.recv().await.unwrap() {
                    break state;
                }
            }
        })
        .await
        .expect("a checkpoint once the sequence ended");
        assert!(
            state.windows(8).any(|w| w == b"halfdone"),
            "{:?}",
            String::from_utf8_lossy(&state)
        );
        session.close();
        let _killed = child.kill().await;
    }

    /// A raw-mode `sh` standing in for zsh with the integration: a three-row prompt with the
    /// marks, ⌃L erases the screen and repaints it at the top, ↩ writes the linefeed and the
    /// `133;C` separately (as zsh does), a `sleep 2` prints nothing while it runs and its
    /// prompt comes ~80 ms after the `D` (a starship's precmd).
    const MARKED_SHELL: &str = r#"
stty raw -echo
CR=$(printf '\r'); FF=$(printf '\f')
p() { printf '\033]133;A\007\r\n~\r\n> \033]133;B\007'; }
p
line=''
while c=$(dd bs=1 count=1 2>/dev/null); do
  if [ "$c" = "$FF" ]; then printf '\033[H\033[2J'; p; line=''
  elif [ "$c" = "$CR" ]; then
    printf '\r\r\n'; sleep 0.02; printf '\033]133;C\007'
    case "$line" in
      sleep*) sleep 2 ;;
      exit) exit 0 ;;
      *) printf 'out\r\n' ;;
    esac
    printf '\033]133;D;0\007\r\033[J'; sleep 0.08; p; line=''
  else printf '%s' "$c"; line="$line$c"; fi
done
"#;

    /// A command that prints nothing while it runs (the e2e's `sleep 6` after ⌘K) is seen
    /// running right after ↩, not when its prompt comes back: the `133;C` that takes the
    /// cursor's row out of the prompt reaches the client as a frame of its own, so the
    /// block tracking sees the cursor below the command while it runs.
    #[tokio::test]
    async fn a_silent_command_after_a_clear_is_seen_running_at_once() {
        fn pump(events: &[TermEvent], state: &mut TermState, effects: &mut Vec<Effect>) {
            for ev in events {
                effects.extend(state.apply(ev.clone()));
            }
        }
        let (session, mut child) = start(&["/bin/sh", "-c", MARKED_SHELL]);
        let me = ClientId::new();
        let (tx, mut rx) = viewer(64);
        session.attach(me, size(40, 6), tx).unwrap();
        let mut state = TermState::new(size(40, 6));
        let mut effects = Vec::new();
        let (events, _) = wait_for(&mut rx, |_, s| text(s).contains("> ")).await;
        pump(&events, &mut state, &mut effects);
        session.request(me, TermRequest::Raw(b"echo hi\r".to_vec())).unwrap();
        let (events, _) = wait_for(&mut rx, |_, s| text(s).contains("out\n\n~\n> ")).await;
        pump(&events, &mut state, &mut effects);
        assert!(effects.iter().any(|e| matches!(e, Effect::CommandFinished { .. })), "{effects:?}");
        effects.clear();
        session.request(me, TermRequest::Clear).unwrap();
        let (events, screen) =
            wait_for(&mut rx, |_, s| !text(s).contains("out") && s.cursor().row == 2).await;
        assert_eq!(text(&screen).trim_end(), "\n~\n>", "{:?}", text(&screen));
        pump(&events, &mut state, &mut effects);
        effects.clear();
        session.request(me, TermRequest::Raw(b"sleep 2\r".to_vec())).unwrap();
        let typed = tokio::time::Instant::now();
        let deadline = typed.checked_add(Duration::from_secs(1)).unwrap();
        while !state.command_running() {
            let ev = tokio::time::timeout_at(deadline, rx.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("not seen running within a second; {effects:?}"))
                .expect("sink closed");
            pump(&[event(&ev)], &mut state, &mut effects);
        }
        assert!(
            effects.iter().any(|e| matches!(e, Effect::CommandStarted(c) if c == "sleep 2")),
            "{effects:?}"
        );
        assert_eq!(state.cursor().row, 3, "the cursor sits below the command while it runs");
        session.request(me, TermRequest::Raw(b"\x04".to_vec())).unwrap();
        session.close();
        let _killed = child.kill().await;
    }

    /// OSC 7 names the directory to every viewer; an OSC 52 write reaches them up to the
    /// clipboard cap and one past it is dropped on the worker.
    #[tokio::test]
    async fn a_directory_and_a_bounded_clipboard_write_reach_the_viewers() {
        let cap = slopty_proto::terminal::MAX_OSC52_BYTES;
        let clip = |n: usize| {
            format!(
                "printf '\\033]52;c;'; head -c {n} /dev/zero | tr '\\0' a | base64 | tr -d '\\n'; printf '\\a'"
            )
        };
        let script = format!(
            "printf '\\033]7;file://localhost/tmp/x\\a'; {}; {}; printf 'MARK\\n'; sleep 30",
            clip(cap),
            clip(cap + 1)
        );
        let (session, mut child) = start(&["/bin/sh", "-c", &script]);
        let (tx, mut rx) = viewer(64);
        session.attach(ClientId::new(), size(40, 6), tx).unwrap();
        let (events, _) = wait_for(&mut rx, |_, s| text(s).contains("MARK")).await;
        let cwd: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                TermEvent::Cwd { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(cwd, ["/tmp/x"]);
        let clips: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                TermEvent::ClipboardWrite { text } => Some(text.len()),
                _ => None,
            })
            .collect();
        assert_eq!(clips, [cap], "the one at the cap arrives, the one past it does not");
        session.close();
        let _killed = child.kill().await;
    }

    /// A shell loop printing `n` numbered lines `pause` seconds apart, then `DONE`.
    fn flood(n: u32, pause: &str) -> String {
        format!(
            "read x; i=0; while [ $i -lt {n} ]; do echo line $i; sleep {pause}; i=$((i+1)); done; echo DONE; sleep 30"
        )
    }

    /// Frames a viewer received, in order: `(seq, full)`.
    fn frames(events: &[TermEvent]) -> Vec<(u64, bool)> {
        events
            .iter()
            .filter_map(|e| match e {
                TermEvent::Frame(f) => Some((f.seq, f.full)),
                _ => None,
            })
            .collect()
    }

    /// Every place a viewer's frames skip a sequence number: `(before, after, after is full)`.
    fn jumps(frames: &[(u64, bool)]) -> Vec<(u64, u64, bool)> {
        frames
            .windows(2)
            .filter_map(|w| match w {
                [(prev, _), (seq, full)] if *seq != prev.wrapping_add(1) => {
                    Some((*prev, *seq, *full))
                }
                _ => None,
            })
            .collect()
    }

    /// What a viewer is sent until its screen shows `DONE`, read as a connection that keeps
    /// up reads it.
    fn until_done(
        mut rx: Viewer,
        after: Duration,
    ) -> tokio::task::JoinHandle<(Vec<TermEvent>, slopty_grid::Screen)> {
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            wait_for(&mut rx, |_, s| text(s).contains("DONE")).await
        })
    }

    /// A viewer joining a busy session is sent every row at the others' sequence number, so
    /// the others' frames run on without a gap: nobody is made to resync, however often
    /// someone joins.
    #[tokio::test]
    async fn viewers_joining_a_busy_session_never_make_the_others_resync() {
        let (session, mut child) = start(&["/bin/sh", "-c", &flood(150, "0.01")]);
        let a = ClientId::new();
        let (tx_a, rx_a) = viewer(4096);
        session.attach(a, size(40, 6), tx_a).unwrap();
        let first = until_done(rx_a, Duration::ZERO);
        session.request(a, TermRequest::Raw(b"\r".to_vec())).unwrap();
        let mut joiners = Vec::new();
        for _ in 0..6 {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let (tx, rx) = viewer(4096);
            session.attach(ClientId::new(), size(40, 6), tx).unwrap();
            joiners.push(until_done(rx, Duration::ZERO));
        }
        let (events, _) = first.await.unwrap();
        let seen = frames(&events);
        assert!(seen.len() > 20, "a busy session: {seen:?}");
        assert_eq!(jumps(&seen), [], "the first viewer never skips a frame");
        for joiner in joiners {
            let (events, _) = joiner.await.unwrap();
            let seen = frames(&events);
            assert!(seen.first().is_some_and(|(_, full)| *full), "{seen:?}");
            assert_eq!(jumps(&seen), [], "a joiner's frames run on from its first");
        }
        session.close();
        let _killed = child.kill().await;
    }

    /// A viewer that stops reading is not dropped and holds nobody back: it misses the diffs
    /// while its frames are on their way, then is sent one whole frame at the others' sequence
    /// number and the diffs after it. Only frames are skipped, so it is never introduced again.
    #[tokio::test]
    async fn a_slow_viewer_is_skipped_then_caught_up_never_dropped() {
        let (session, mut child) = start(&["/bin/sh", "-c", &flood(150, "0.01")]);
        let fast = ClientId::new();
        let (tx_fast, rx_fast) = viewer(4096);
        session.attach(fast, size(40, 6), tx_fast).unwrap();
        let fast_seen = until_done(rx_fast, Duration::ZERO);
        let slow = ClientId::new();
        let (tx_slow, rx_slow) = viewer(8);
        session.attach(slow, size(40, 6), tx_slow).unwrap();
        // Long enough for some fifty frames.
        let slow_seen = until_done(rx_slow, Duration::from_millis(500));
        session.request(fast, TermRequest::Raw(b"\r".to_vec())).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(session.snapshot().await.unwrap().viewers, 2, "the slow viewer stays");
        let (events, screen) = slow_seen.await.unwrap();
        let seen = jumps(&frames(&events));
        assert!(!seen.is_empty(), "the slow viewer skipped frames while it did not read");
        assert!(seen.iter().all(|&(_, _, full)| full), "each skip ends in a whole frame: {seen:?}");
        let introduced = events.iter().filter(|e| matches!(e, TermEvent::Driver { .. })).count();
        assert_eq!(introduced, 0, "never introduced again: {events:?}");
        assert!(text(&screen).contains("line 149\nDONE"), "{}", text(&screen));
        let (events, _) = fast_seen.await.unwrap();
        assert_eq!(jumps(&frames(&events)), [], "the fast viewer is not held back");
        session.close();
        let _killed = child.kill().await;
    }

    /// A paste far larger than the tty's input queue into a program that echoes as it reads:
    /// the actor keeps reading the echo while the paste goes in, so neither side waits on the
    /// other forever.
    #[tokio::test]
    async fn a_large_paste_into_an_echoing_program_does_not_deadlock() {
        // `tr` echoes each line back in capitals, after the tty's own echo of it.
        let (session, mut child) = start(&["/usr/bin/tr", "a-z", "A-Z"]);
        let me = ClientId::new();
        let (tx, mut rx) = viewer(4096);
        session.attach(me, size(40, 6), tx).unwrap();
        let mut paste = format!("{}\n", "x".repeat(99)).repeat(2_600);
        paste.push_str("end\n");
        session.request(me, TermRequest::Paste(paste)).unwrap();
        let (_, screen) = wait_for(&mut rx, |_, s| text(s).contains("END")).await;
        assert!(text(&screen).contains("END"), "{}", text(&screen));
        assert_eq!(session.snapshot().await.unwrap().viewers, 1, "the actor answers");
        session.close();
        let _killed = child.kill().await;
    }

    /// The exit the viewers are told is the status the child exited with, once its reaper
    /// says so, not a guess at the end of its output.
    #[tokio::test]
    async fn the_viewers_are_told_the_real_exit_status_of_the_child() {
        let (session, mut child) = start(&["/bin/sh", "-c", "read x; exit 3"]);
        let me = ClientId::new();
        let (tx, mut rx) = viewer(64);
        session.attach(me, size(40, 6), tx).unwrap();
        session.request(me, TermRequest::Raw(b"\r".to_vec())).unwrap();
        let status = child.wait().await.unwrap().code().unwrap();
        assert_eq!(status, 3);
        session.exited(status);
        let (events, _) =
            wait_for(&mut rx, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Exited { .. })))
                .await;
        let exits: Vec<i32> = events
            .iter()
            .filter_map(|e| match e {
                TermEvent::Exited { status } => Some(*status),
                _ => None,
            })
            .collect();
        assert_eq!(exits, [3]);
        assert_eq!(session.snapshot().await.unwrap().exited, Some(3));
        session.close();
    }

    /// A program that begins synchronized output and never ends it is let go after the
    /// timeout, whether or not it writes again: its screen does not freeze.
    #[tokio::test]
    async fn a_hold_that_is_never_released_times_out_without_more_output() {
        let (session, mut child) =
            start(&["/bin/sh", "-c", "read x; printf '\\033[?2026hheld'; sleep 30"]);
        let me = ClientId::new();
        let (tx, mut rx) = viewer(64);
        session.attach(me, size(40, 6), tx).unwrap();
        let _first =
            wait_for(&mut rx, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Frame(_)))).await;
        let asked = tokio::time::Instant::now();
        session.request(me, TermRequest::Raw(b"\r".to_vec())).unwrap();
        let (_, screen) = wait_for(&mut rx, |_, s| text(s).contains("held")).await;
        let waited = asked.elapsed();
        assert!(text(&screen).contains("held"));
        assert!(
            waited >= Duration::from_millis(900) && waited < Duration::from_secs(3),
            "shown after the hold timed out, not before and not never: {waited:?}"
        );
        session.close();
        let _killed = child.kill().await;
    }

    /// A resize is told to ptyd, and the checkpoint after it is taken at the new size, so a
    /// worker that replaces this one replays at the size the program draws for.
    #[tokio::test]
    async fn a_resize_reaches_ptyd_and_the_next_checkpoint_is_at_it() {
        let (session, mut child, mut taps) =
            start_tapped(&["/bin/sh", "-c", "read x; exit 0"], Vec::new());
        let me = ClientId::new();
        let (tx, mut rx) = viewer(64);
        session.attach(me, size(40, 6), tx).unwrap();
        let _first =
            wait_for(&mut rx, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Frame(_)))).await;
        session.request(me, TermRequest::Resize(size(50, 8))).unwrap();
        let deadline = tokio::time::Instant::now().checked_add(Duration::from_secs(5)).unwrap();
        let mut told = None;
        loop {
            match tokio::time::timeout_at(deadline, taps.recv()).await.unwrap().unwrap() {
                Tap::Resize { size, .. } => told = Some(size),
                Tap::Checkpoint { .. } if told.is_some() => break,
                Tap::Checkpoint { .. } | Tap::Output { .. } => {}
            }
        }
        assert_eq!(told, Some(size(50, 8)));
        session.request(me, TermRequest::Raw(b"\r".to_vec())).unwrap();
        child.wait().await.unwrap();
        session.close();
    }

    /// Reads a sink as a connection that keeps up would, stamping each event as it arrives.
    fn stamped(
        mut rx: mpsc::Receiver<Outbound>,
    ) -> mpsc::UnboundedReceiver<(tokio::time::Instant, TermEvent)> {
        let (tx, stamps) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(out) = rx.recv().await {
                if tx.send((tokio::time::Instant::now(), event(&out))).is_err() {
                    break;
                }
            }
        });
        stamps
    }

    /// `(p50, p90, max)` of `samples`, in milliseconds.
    fn spread(samples: &mut [Duration]) -> (f64, f64, f64) {
        samples.sort_unstable();
        let at = |percent: usize| {
            samples[samples.len().saturating_sub(1).saturating_mul(percent) / 100].as_secs_f64()
                * 1e3
        };
        (at(50), at(90), at(100))
    }

    /// A key typed while the program keeps printing (a spinner, a TUI redrawing) has its echo
    /// framed as it is read, not at the next paced frame, while the printing itself stays
    /// paced.
    #[tokio::test]
    async fn an_echo_beside_a_flood_is_not_held_to_the_frame_pace() {
        let (session, mut child) = start(&[
            "/bin/sh",
            "-c",
            "i=0; while [ $i -lt 3000 ]; do printf .; sleep 0.002; i=$((i+1)); done & exec cat",
        ]);
        let me = ClientId::new();
        let (tx, rx) = mpsc::channel(4096);
        session.attach(me, size(40, 6), tx).unwrap();
        let mut stamps = stamped(rx);
        let next = async |stamps: &mut mpsc::UnboundedReceiver<_>| {
            tokio::time::timeout(Duration::from_secs(5), stamps.recv())
                .await
                .expect("an event")
                .expect("sink open")
        };
        let settled = tokio::time::Instant::now() + Duration::from_millis(300);
        while next(&mut stamps).await.0 < settled {}
        let counted_until = tokio::time::Instant::now() + Duration::from_millis(400);
        let mut paced = 0_u32;
        loop {
            let (at, ev) = next(&mut stamps).await;
            if at >= counted_until {
                break;
            }
            paced += u32::from(matches!(ev, TermEvent::Frame(_)));
        }
        let mut waits = Vec::new();
        for seq in 1..=40_u64 {
            // Keys land at every phase of the pace.
            tokio::time::sleep(Duration::from_millis(11 + seq % 7)).await;
            let asked = tokio::time::Instant::now();
            session.request(me, key(seq, KeyCode::X, "x")).unwrap();
            loop {
                let (at, ev) = next(&mut stamps).await;
                if let TermEvent::Frame(f) = ev
                    && f.input_ack >= seq
                {
                    waits.push(at.duration_since(asked));
                    break;
                }
            }
        }
        let (p50, p90, max) = spread(&mut waits);
        eprintln!(
            "MEASURE echo beside a flood: key -> acking frame p50 {p50:.2} p90 {p90:.2} max {max:.2} ms; flood frames in 400 ms: {paced}"
        );
        assert!((25..=55).contains(&paced), "the flood is on and paced to 8 ms: {paced}");
        assert!(p90 < 4.0, "an echo is not held for the pace: p90 {p90:.2} ms");
        session.close();
        let _killed = child.kill().await;
    }

    /// A viewer whose connection drains slower than the program writes is sent frames as its
    /// connection takes them: what it shows is a frame or two old, not a queue of seconds, and
    /// the last frame shows where the program ended.
    #[tokio::test]
    async fn a_throttled_viewer_is_a_frame_or_two_behind_not_seconds() {
        /// The link, in bytes a second.
        const RATE: u64 = 250_000;
        let script = "read x; i=0; while [ $i -lt 120 ]; do j=0; while [ $j -lt 15 ]; do \
                      echo \"line $i.$j the quick brown fox jumps over the lazy dog again\"; \
                      j=$((j+1)); done; sleep 0.01; i=$((i+1)); done; echo DONE; sleep 30";
        let (session, mut child) = start(&["/bin/sh", "-c", script]);
        let me = ClientId::new();
        // The depth a connection gives its sink.
        let (tx, mut rx) = mpsc::channel::<Outbound>(256);
        let probe = tx.clone();
        session.attach(me, size(80, 24), tx).unwrap();
        let link = tokio::spawn(async move {
            let mut state = TermState::new(size(80, 24));
            let started = tokio::time::Instant::now();
            let (mut carried, mut queued, mut frames) = (0_usize, 0_usize, 0_usize);
            loop {
                let out = rx.recv().await.expect("sink open");
                queued = queued.max(probe.max_capacity() - probe.capacity());
                carried += out.wire().len();
                let ev = event(&out);
                // The event is on the link until its last byte has gone: the connection
                // holds it that long, as `send_raw` does.
                let on_link = u64::try_from(carried).unwrap().saturating_mul(1_000_000) / RATE;
                tokio::time::sleep_until(started + Duration::from_micros(on_link)).await;
                drop(out);
                let frame = matches!(ev, TermEvent::Frame(_));
                let _effects = state.apply(ev);
                if frame {
                    frames += 1;
                    if text(state.screen()).contains("DONE") {
                        return (tokio::time::Instant::now(), queued, carried / frames);
                    }
                }
            }
        });
        session.request(me, TermRequest::Raw(b"\r".to_vec())).unwrap();
        let ended = loop {
            if let session::Text::Screen { screen, .. } =
                session.read(session::Read::Screen).await.unwrap()
                && screen.rows.iter().any(|r| r.contains("DONE"))
            {
                break tokio::time::Instant::now();
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        };
        let (shown, queued, frame_bytes) =
            tokio::time::timeout(Duration::from_secs(60), link).await.unwrap().unwrap();
        let stale = shown.duration_since(ended);
        eprintln!(
            "MEASURE throttled viewer at {RATE} B/s: shown {:.0} ms after the program ended; at most {queued} events queued; {frame_bytes} B a frame",
            stale.as_secs_f64() * 1e3
        );
        assert!(
            queued <= 3,
            "a frame or two (and a marker) wait for the link, not a queue: {queued}"
        );
        assert!(stale < Duration::from_millis(600), "the end shows promptly: {stale:?}");
        session.close();
        let _killed = child.kill().await;
    }

    /// The same flood behind a QUIC stream: the connection's write returns once the frame is
    /// in noq's send buffer, which takes up to the stream window before the link has carried
    /// any of it. The client applies each event as the link delivers it and sends back what
    /// its state asks the worker for, as the app does.
    #[tokio::test]
    async fn a_throttled_viewer_behind_the_stream_window_is_under_a_second_behind() {
        /// The link, in bytes a second.
        const RATE: u64 = 250_000;
        /// noq's default stream receive window: what a stream may hold unread by the peer.
        const WINDOW: usize = 1_250_000;
        let script = "read x; i=0; while [ $i -lt 120 ]; do j=0; while [ $j -lt 15 ]; do \
                      echo \"line $i.$j the quick brown fox jumps over the lazy dog again\"; \
                      j=$((j+1)); done; sleep 0.01; i=$((i+1)); done; echo DONE; sleep 30";
        let (session, mut child) = start(&["/bin/sh", "-c", script]);
        let me = ClientId::new();
        let (tx, mut rx) = mpsc::channel::<Outbound>(256);
        session.attach(me, size(80, 24), tx).unwrap();
        let (wire_tx, mut wire_rx) = mpsc::unbounded_channel::<(tokio::time::Instant, TermEvent)>();
        // The connection: a write waits only for room in the stream window, and the frame's
        // credit goes back as soon as it is written.
        let writer = tokio::spawn(async move {
            let mut buffered: std::collections::VecDeque<(tokio::time::Instant, usize)> =
                std::collections::VecDeque::new();
            let mut last = tokio::time::Instant::now();
            let mut most = 0_usize;
            while let Some(out) = rx.recv().await {
                let len = out.wire().len();
                loop {
                    let now = tokio::time::Instant::now();
                    while buffered.front().is_some_and(|&(at, _)| at <= now) {
                        buffered.pop_front();
                    }
                    let held: usize = buffered.iter().map(|&(_, n)| n).sum();
                    most = most.max(held);
                    if held + len <= WINDOW || buffered.is_empty() {
                        break;
                    }
                    let (at, _) = buffered.front().copied().unwrap();
                    tokio::time::sleep_until(at).await;
                }
                let now = tokio::time::Instant::now();
                let on_link = Duration::from_micros(
                    u64::try_from(len).unwrap().saturating_mul(1_000_000) / RATE,
                );
                last = last.max(now) + on_link;
                buffered.push_back((last, len));
                let ev = event(&out);
                drop(out);
                if wire_tx.send((last, ev)).is_err() {
                    break;
                }
            }
            most
        });
        let client = {
            let session = session.clone();
            tokio::spawn(async move {
                let mut state = TermState::new(size(80, 24));
                while let Some((at, ev)) = wire_rx.recv().await {
                    tokio::time::sleep_until(at).await;
                    for effect in state.apply(ev) {
                        if let Effect::Request(req) = effect {
                            let _sent = session.request(me, req);
                        }
                    }
                    if state.screen().lines().iter().any(|l| l.text().contains("DONE")) {
                        return tokio::time::Instant::now();
                    }
                }
                panic!("the stream ended before DONE");
            })
        };
        session.request(me, TermRequest::Raw(b"\r".to_vec())).unwrap();
        let ended = loop {
            if let session::Text::Screen { screen, .. } =
                session.read(session::Read::Screen).await.unwrap()
                && screen.rows.iter().any(|r| r.contains("DONE"))
            {
                break tokio::time::Instant::now();
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        };
        let shown = tokio::time::timeout(Duration::from_secs(60), client).await.unwrap().unwrap();
        let stale = shown.duration_since(ended);
        session.close();
        let most = writer.await.unwrap();
        eprintln!(
            "MEASURE viewer behind a {WINDOW} B stream window at {RATE} B/s: shown {:.0} ms after the program ended; at most {most} B in the stream buffer",
            stale.as_secs_f64() * 1e3
        );
        let bound = slopty_proto::terminal::FRAMES_UNREACHED_BYTES;
        assert!(most < bound + bound / 2, "the confirmed frames bound the buffer: {most} B");
        assert!(stale < Duration::from_secs(1), "the end shows promptly: {stale:?}");
        let _killed = child.kill().await;
    }

    /// A sink that closes does not say whether its client left or is attaching again (the
    /// connection lets go of the old sink before the new attach arrives), so the driver's
    /// seat waits: another viewer is not handed the size until the connection detaches that
    /// sink, and the driver attaching again keeps it.
    #[tokio::test]
    async fn a_closed_sink_keeps_the_drivers_seat_until_its_connection_detaches_it() {
        let (session, mut child) = start(&["/bin/sh", "-c", "cat"]);
        let (a, b) = (ClientId::new(), ClientId::new());
        let (old_tx, mut old_a) = viewer(64);
        let (tx_b, mut rx_b) = viewer(64);
        session.attach(a, size(40, 6), old_tx.clone()).unwrap();
        wait_for(&mut old_a, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Driver { you: true }))
        })
        .await;
        session.attach(b, size(60, 10), tx_b).unwrap();
        wait_for(&mut rx_b, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Frame(f) if f.full)))
            .await;
        let driven_by_b = |events: &[TermEvent]| {
            events.iter().any(|e| {
                matches!(e, TermEvent::Driver { you: true } | TermEvent::Resized { cols: 60, .. })
            })
        };
        // A's connection lets go of its sink, as a re-attach does before the attach lands.
        drop(old_a);
        session.request(b, TermRequest::Raw(b"x".to_vec())).unwrap();
        let (events, _) = wait_for(&mut rx_b, |_, s| text(s).contains('x')).await;
        assert!(!driven_by_b(&events), "{events:?}");
        let snap = session.snapshot().await.unwrap();
        assert_eq!((snap.viewers, snap.size.cols), (1, 40), "a's viewer went, its size stays");
        // The attach lands: a still drives.
        let (new_tx, mut new_a) = viewer(64);
        session.attach(a, size(40, 6), new_tx.clone()).unwrap();
        wait_for(&mut new_a, |ev, _| {
            ev.iter().any(|e| matches!(e, TermEvent::Driver { you: true }))
        })
        .await;
        // The old connection's detach names the old sink: nothing moves.
        session.detach_sink(a, &old_tx).unwrap();
        session.request(b, TermRequest::Raw(b"y".to_vec())).unwrap();
        let (events, _) = wait_for(&mut rx_b, |_, s| text(s).contains('y')).await;
        assert!(!driven_by_b(&events), "{events:?}");
        // A's connection goes for good: its sink closes, then its detach passes the seat on.
        drop(new_a);
        session.request(b, TermRequest::Raw(b"z".to_vec())).unwrap();
        let (events, _) = wait_for(&mut rx_b, |_, s| text(s).contains('z')).await;
        assert!(!driven_by_b(&events), "{events:?}");
        session.detach_sink(a, &new_tx).unwrap();
        let (events, _) = wait_for(&mut rx_b, |ev, _| driven_by_b(ev)).await;
        assert!(events.iter().any(|e| matches!(e, TermEvent::Driver { you: true })));
        session.close();
        let _killed = child.kill().await;
    }

    /// A viewer that stopped reading still gets the rows it asked for once it reads again:
    /// only frames are coalesced for it, never a reply.
    #[tokio::test]
    async fn a_reply_reaches_a_viewer_that_fell_behind_the_frames() {
        let (session, mut child) = start(&["/bin/sh", "-c", &flood(150, "0.01")]);
        let me = ClientId::new();
        let (tx, mut rx) = viewer(8);
        session.attach(me, size(40, 6), tx).unwrap();
        session.request(me, TermRequest::Raw(b"\r".to_vec())).unwrap();
        // Long enough for some twenty frames, more than the sink holds.
        tokio::time::sleep(Duration::from_millis(200)).await;
        session.request(me, TermRequest::FetchLines { start: LineIndex(0), count: 4 }).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let (events, _) =
            wait_for(&mut rx, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Lines { .. })))
                .await;
        assert!(
            events.iter().any(|e| matches!(e, TermEvent::Lines { lines, .. } if !lines.is_empty()))
        );
        session.close();
        let _killed = child.kill().await;
    }
}
