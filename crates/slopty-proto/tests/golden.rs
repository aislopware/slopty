//! Golden byte snapshots of representative messages. A changed snapshot is a protocol change:
//! bump [`slopty_proto::PROTOCOL_VERSION`] and accept it deliberately (`cargo insta review`).

#[cfg(test)]
mod golden {
    use slopty_core::{ClientId, MonoTime, SessionId, StreamId};
    use slopty_grid::{Cursor, CursorShape, Line, RowUpdate, Style, TermModes};
    use slopty_proto::handshake::{Caps, ClientKind, Hello};
    use slopty_proto::input::{KeyAction, KeyCode, KeyEvent, Mods};
    use slopty_proto::screen::{Feedback, ScreenEvent, ScreenRequest};
    use slopty_proto::terminal::{Frame, SearchMatch, TermEvent, TermRequest};
    use slopty_proto::{ClientMsg, HostMsg, PROTOCOL_VERSION, codec};
    use uuid::Uuid;

    fn session() -> SessionId {
        SessionId::from_uuid(Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10))
    }

    fn hex(bytes: &[u8]) -> String {
        bytes
            .chunks(16)
            .map(|row| row.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[track_caller]
    fn snap<T: serde::Serialize>(name: &str, msg: &T) {
        let bytes = codec::encode(msg).expect("encodes");
        insta::assert_snapshot!(name, hex(&bytes));
    }

    #[test]
    fn hello() {
        snap(
            "client_hello",
            &ClientMsg::Hello(Hello {
                protocol: PROTOCOL_VERSION,
                client: ClientId::from_uuid(Uuid::from_u128(0x42)),
                kind: ClientKind::IPad,
                name: "iPad".to_owned(),
                app_version: "0.1.0".to_owned(),
                caps: Caps::HEVC | Caps::OPUS | Caps::PREDICTION,
                pair_token: None,
            }),
        );
    }

    #[test]
    fn key_press() {
        snap(
            "client_key",
            &ClientMsg::Term {
                session: session(),
                req: TermRequest::Key(KeyEvent {
                    seq: 7,
                    action: KeyAction::Press,
                    code: KeyCode::A,
                    mods: Mods::SHIFT,
                    consumed_mods: Mods::SHIFT,
                    text: Some("A".to_owned()),
                    unshifted: Some('a'),
                    composing: false,
                }),
            },
        );
    }

    #[test]
    fn ping_pong() {
        snap("client_ping", &ClientMsg::Ping { sent_at: MonoTime::from_nanos(1_000_000) });
        snap("host_pong", &HostMsg::Pong { sent_at: MonoTime::from_nanos(1_000_000) });
    }

    #[test]
    fn search() {
        snap(
            "client_search",
            &ClientMsg::Term {
                session: session(),
                req: TermRequest::Search { needle: "fox".to_owned(), max: 2000, regex: false },
            },
        );
        snap(
            "host_search_invalid",
            &TermEvent::SearchInvalid {
                needle: "(".to_owned(),
                message: "unclosed group".to_owned(),
            },
        );
        snap(
            "host_matches",
            &TermEvent::Matches {
                needle: "fox".to_owned(),
                total: 3,
                matches: vec![
                    SearchMatch { line: slopty_grid::LineIndex(4), col: 16, len: 3 },
                    SearchMatch { line: slopty_grid::LineIndex(9), col: 0, len: 3 },
                ],
            },
        );
    }

    #[test]
    fn feedback() {
        let nack =
            Feedback::Nack { stream: StreamId(7), frame: 0x0102_0304, fragments: vec![2, 5] };
        let bytes = codec::encode_body(&nack).expect("encodes");
        insta::assert_snapshot!("client_nack", hex(&bytes));
        let refresh = Feedback::Refresh { stream: StreamId(7), last_good_frame: 300 };
        let bytes = codec::encode_body(&refresh).expect("encodes");
        insta::assert_snapshot!("client_refresh", hex(&bytes));
    }

    #[test]
    fn clipboard() {
        snap(
            "client_clipboard",
            &ClientMsg::Screen(ScreenRequest::Clipboard { text: "fox".to_owned() }),
        );
        snap("host_clipboard", &HostMsg::Screen(ScreenEvent::Clipboard { text: "fox".to_owned() }));
    }

    #[test]
    fn frame() {
        let mut line = Line::from_text("$ ls", 8, Style::DEFAULT);
        line.cells[1].style.flags = slopty_grid::StyleFlags::BOLD;
        snap(
            "host_frame",
            &TermEvent::Frame(Frame {
                seq: 3,
                full: false,
                epoch: 1,
                cols: 8,
                rows: 2,
                cursor: Cursor {
                    row: 0,
                    col: 4,
                    shape: CursorShape::Bar,
                    visible: true,
                    blink: true,
                },
                modes: TermModes::BRACKETED_PASTE | TermModes::FOCUS_EVENTS,
                oldest_line: slopty_grid::LineIndex(0),
                first_visible_line: slopty_grid::LineIndex(10),
                total_lines: 12,
                input_ack: 7,
                updates: vec![RowUpdate { row: 0, line }],
            }),
        );
    }
}
