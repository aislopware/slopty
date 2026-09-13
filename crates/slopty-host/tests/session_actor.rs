//! The session actor against a real shell on an in-process PTY (no ptyd).

#[cfg(test)]
mod actor {
    use std::time::Duration;

    use slopty_core::{ClientId, SessionId};
    use slopty_grid::LineIndex;
    use slopty_host::session::{self, SessionStart, Tap};
    use slopty_proto::input::{CellMetrics, KeyAction, KeyCode, KeyEvent, Mods};
    use slopty_proto::terminal::{Frame, TermColors, TermEvent, TermRequest, TermSize};
    use slopty_pty::{Pty, SpawnSpec};
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
        })
        .unwrap();
        (handle, child, tap_rx)
    }

    /// Apply frames onto a local screen model and return its text once `pred` holds.
    async fn wait_for(
        rx: &mut mpsc::Receiver<TermEvent>,
        mut pred: impl FnMut(&[TermEvent], &slopty_grid::Screen) -> bool,
    ) -> (Vec<TermEvent>, slopty_grid::Screen) {
        let mut seen = Vec::new();
        let mut screen = slopty_grid::Screen::new(40, 6);
        let deadline = tokio::time::Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        loop {
            let ev = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .unwrap_or_else(|_| {
                    panic!("timeout waiting for events; {seen:?}\nscreen:\n{}", text(&screen))
                })
                .expect("sink closed");
            if let TermEvent::Frame(f) = &ev {
                apply(&mut screen, f);
            }
            seen.push(ev);
            if pred(&seen, &screen) {
                return (seen, screen);
            }
        }
    }

    fn apply(screen: &mut slopty_grid::Screen, f: &Frame) {
        if (screen.cols(), screen.rows()) != (f.cols, f.rows) {
            screen.resize(f.cols, f.rows);
        }
        for u in &f.updates {
            screen.apply(u.clone()).unwrap();
        }
        *screen.cursor_mut() = f.cursor;
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
        let (tx, mut rx) = mpsc::channel(64);
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
        let _exit =
            wait_for(&mut rx, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Exited { .. })))
                .await;
        child.wait().await.unwrap();
        session.close();
    }

    /// Every read goes to the tap as it is, and once the shell is quiet a checkpoint follows
    /// that a fresh actor can start from: the second actor shows the first one's screen.
    #[tokio::test]
    async fn output_is_tapped_and_a_quiet_session_checkpoints() {
        let (session, mut child, mut taps) = start_tapped(&["/bin/sh", "-c", "cat"], Vec::new());
        let me = ClientId::new();
        let (tx, mut rx) = mpsc::channel(64);
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
                Tap::Checkpoint { .. } => {}
            }
        };
        let text_out = String::from_utf8_lossy(&output);
        assert!(text_out.contains("tapped-line\r\ntapped-line"), "tapped: {text_out:?}");

        let (next, mut cat2, mut taps2) = start_tapped(&["/bin/sh", "-c", "cat"], checkpoint);
        let (tx2, mut rx2) = mpsc::channel(64);
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
        let (tx_a, mut rx_a) = mpsc::channel(64);
        let (tx_b, mut rx_b) = mpsc::channel(64);
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
        let (tx_a, mut rx_a) = mpsc::channel(64);
        let (tx_b, mut rx_b) = mpsc::channel(64);
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
        let (tx_a, mut rx_a) = mpsc::channel(64);
        let (tx_b, mut rx_b) = mpsc::channel(64);
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
        let (tx_a, mut rx_a) = mpsc::channel(64);
        let (tx_b, mut rx_b) = mpsc::channel(64);
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

    /// A restored session (a checkpoint from the last host) tells its first attach the
    /// program's colours and the directory the replay carried, ahead of the frame.
    #[tokio::test]
    async fn a_restored_session_tells_the_first_attach_what_the_replay_said() {
        let mut e = slopty_engine::GhosttyEngine::new(slopty_engine::EngineConfig {
            size: size(40, 6),
            scrollback_lines: 100,
        })
        .unwrap();
        slopty_engine::VtEngine::write(
            &mut e,
            b"\x1b]7;file:///tmp\x1b\\\x1b]11;#282c34\x1b\\hello",
        );
        let checkpoint = e.checkpoint().unwrap();
        let (session, mut child, _tap) =
            start_tapped(&["/bin/sh", "-c", "read x; exit 0"], checkpoint);
        let a = ClientId::new();
        let (tx_a, mut rx_a) = mpsc::channel(64);
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
        let (tx_other, mut rx_other) = mpsc::channel(64);
        let (tx_opener, mut rx_opener) = mpsc::channel(64);
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
        let (old_tx, mut old_rx) = mpsc::channel(64);
        let (new_tx, mut new_rx) = mpsc::channel(64);
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
        let (b_tx, mut b_rx) = mpsc::channel(64);
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
        let (tx, mut rx) = mpsc::channel(64);
        session.attach(me, size(40, 6), tx).unwrap();
        wait_for(&mut rx, |ev, _| ev.iter().any(|e| matches!(e, TermEvent::Frame(_)))).await;
        session.close();
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("the child exits once the master is closed")
            .unwrap();
        assert!(!status.success() || status.code() == Some(0), "{status:?}");
        assert!(matches!(session.snapshot().await, Err(slopty_host::HostError::SessionClosed)));
    }

    /// A quiet spell inside an escape sequence does not checkpoint: the state would replace
    /// the sequence's head and its tail would print as text after a restart.
    #[tokio::test]
    async fn a_checkpoint_waits_for_the_end_of_an_escape_sequence() {
        let (session, mut child, mut taps) = start_tapped(
            &["/bin/sh", "-c", "printf '\\033]0;half'; sleep 2; printf 'done\\033\\\\'; sleep 30"],
            Vec::new(),
        );
        let (tx, _rx) = mpsc::channel(64);
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
                Tap::Checkpoint { .. } => {}
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
        fn pump(
            events: &[TermEvent],
            state: &mut slopty_client::term::TermState,
            effects: &mut Vec<slopty_client::term::Effect>,
        ) {
            for ev in events {
                effects.extend(state.apply(ev.clone()));
            }
        }
        let (session, mut child) = start(&["/bin/sh", "-c", MARKED_SHELL]);
        let me = ClientId::new();
        let (tx, mut rx) = mpsc::channel(64);
        session.attach(me, size(40, 6), tx).unwrap();
        let mut state = slopty_client::term::TermState::new(size(40, 6));
        let mut effects = Vec::new();
        let (events, _) = wait_for(&mut rx, |_, s| text(s).contains("> ")).await;
        pump(&events, &mut state, &mut effects);
        session.request(me, TermRequest::Raw(b"echo hi\r".to_vec())).unwrap();
        let (events, _) = wait_for(&mut rx, |_, s| text(s).contains("out\n\n~\n> ")).await;
        pump(&events, &mut state, &mut effects);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, slopty_client::term::Effect::CommandFinished { .. })),
            "{effects:?}"
        );
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
            let ev = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .unwrap_or_else(|_| panic!("not seen running within a second; {effects:?}"))
                .expect("sink closed");
            pump(std::slice::from_ref(&ev), &mut state, &mut effects);
        }
        assert!(
            effects.iter().any(
                |e| matches!(e, slopty_client::term::Effect::CommandStarted(c) if c == "sleep 2")
            ),
            "{effects:?}"
        );
        assert_eq!(state.cursor().row, 3, "the cursor sits below the command while it runs");
        session.request(me, TermRequest::Raw(b"\x04".to_vec())).unwrap();
        session.close();
        let _killed = child.kill().await;
    }

    /// OSC 7 names the directory to every viewer; an OSC 52 write reaches them up to the
    /// clipboard cap and one past it is dropped on the host.
    #[tokio::test]
    async fn a_directory_and_a_bounded_clipboard_write_reach_the_viewers() {
        let cap = slopty_proto::screen::MAX_CLIPBOARD_BYTES;
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
        let (tx, mut rx) = mpsc::channel(64);
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
}
