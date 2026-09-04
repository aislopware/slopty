//! The session actor against a real shell on an in-process PTY (no ptyd).

#[cfg(test)]
mod actor {
    use std::time::Duration;

    use slopty_core::{ClientId, SessionId};
    use slopty_grid::LineIndex;
    use slopty_host::session::{self, SessionStart};
    use slopty_proto::input::{CellMetrics, KeyAction, KeyCode, KeyEvent, Mods};
    use slopty_proto::terminal::{Frame, TermEvent, TermRequest, TermSize};
    use slopty_pty::{Pty, SpawnSpec};
    use tokio::sync::mpsc;

    fn size(cols: u16, rows: u16) -> TermSize {
        TermSize { cols, rows, metrics: CellMetrics { cell_width: 8, cell_height: 16 } }
    }

    fn start(command: &[&str]) -> (session::SessionHandle, tokio::process::Child) {
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
            backlog: Vec::new(),
            size: size(40, 6),
            scrollback_lines: 1000,
        })
        .unwrap();
        (handle, child)
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
        screen.lines().iter().map(slopty_grid::Line::text).collect::<Vec<_>>().join("\n")
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
}
