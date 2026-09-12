//! The session actor against a real shell on an in-process PTY (no ptyd).

#[cfg(test)]
mod actor {
    use std::time::Duration;

    use slopty_core::{ClientId, SessionId};
    use slopty_grid::LineIndex;
    use slopty_host::session::{self, SessionStart, Tap};
    use slopty_proto::input::{CellMetrics, KeyAction, KeyCode, KeyEvent, Mods};
    use slopty_proto::terminal::{Frame, TermEvent, TermRequest, TermSize};
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
                .expect("timeout waiting for events")
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
}
