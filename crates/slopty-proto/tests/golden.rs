//! Golden byte snapshots of representative messages. A changed snapshot is a protocol change:
//! bump [`slopty_proto::PROTOCOL_VERSION`] and accept it deliberately (`cargo insta review`).

#[cfg(test)]
mod golden {
    use slopty_core::{ClientId, MonoTime, SessionId, StreamId};
    use slopty_grid::{
        Cursor, CursorShape, Hyperlink, Line, RowUpdate, SemanticMark, Style, TermModes,
    };
    use slopty_proto::handshake::{Caps, ClientKind, Hello};
    use slopty_proto::input::{KeyAction, KeyCode, KeyEvent, Mods};
    use slopty_proto::screen::{Feedback, RateVerdict, ReceiverReport, ScreenEvent, ScreenRequest};
    use slopty_proto::terminal::{
        ColorOverrides, Frame, PixelRect, Placement, SearchMatch, TermColors, TermEvent,
        TermRequest,
    };
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
            }),
        );
    }

    #[test]
    fn look_and_presence() {
        let view = slopty_proto::canvas::Rect { x: -120.0, y: 40.0, w: 1440.0, h: 900.0 };
        snap("client_look", &ClientMsg::Look { view: Some(view) });
        snap(
            "host_presence",
            &HostMsg::Canvas(slopty_proto::canvas::CanvasSync::Presence {
                client: ClientId::from_uuid(Uuid::from_u128(0x42)),
                kind: ClientKind::IPhone,
                name: "iPhone".to_owned(),
                view: Some(view),
            }),
        );
    }

    #[test]
    fn point_and_pointed() {
        let item = slopty_core::ItemId::from_uuid(Uuid::from_u128(0x77));
        snap("client_point", &ClientMsg::Point { item });
        snap(
            "host_pointed",
            &HostMsg::Canvas(slopty_proto::canvas::CanvasSync::Pointed {
                client: ClientId::from_uuid(Uuid::from_u128(0x42)),
                name: "iPhone".to_owned(),
                item,
            }),
        );
    }

    #[test]
    fn colors() {
        let mut ansi = [[0_u8; 3]; 16];
        for (i, c) in (0_u8..).zip(ansi.iter_mut()) {
            *c = [i, i.wrapping_mul(16), 255_u8.wrapping_sub(i)];
        }
        snap(
            "client_colors",
            &ClientMsg::Term {
                session: session(),
                req: TermRequest::Colors(TermColors {
                    fg: [0xe6, 0xe6, 0xe6],
                    bg: [0x0e, 0x0f, 0x12],
                    cursor: [0x8a, 0xb4, 0xf8],
                    ansi,
                }),
            },
        );
        snap(
            "host_term_colors",
            &TermEvent::Colors(ColorOverrides {
                fg: None,
                bg: Some([0x28, 0x2c, 0x34]),
                cursor: Some([0xff, 0xff, 0xff]),
                palette: vec![(1, [0xe0, 0x6c, 0x75]), (17, [0x00, 0x00, 0x5f])],
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
                    option_as_alt: false,
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
    fn clear() {
        snap("client_term_clear", &ClientMsg::Term { session: session(), req: TermRequest::Clear });
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
    fn agent() {
        use slopty_proto::agent::{AgentEvent, AgentKind, AgentSource, AgentStatus, BlockReason};
        snap(
            "host_agent_hook",
            &HostMsg::Agent(AgentEvent {
                session: session(),
                kind: AgentKind::ClaudeCode,
                status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() }),
                agent_session: Some("6f1b".to_owned()),
                detail: Some("$ cargo test".to_owned()),
                attention: true,
                source: AgentSource::Hook,
            }),
        );
        // The same session attributed without hooks: the pill is the same, the source is not.
        snap(
            "host_agent_process",
            &HostMsg::Agent(AgentEvent {
                session: session(),
                kind: AgentKind::ClaudeCode,
                status: AgentStatus::Idle,
                agent_session: None,
                detail: None,
                attention: false,
                source: AgentSource::Process,
            }),
        );
        snap("client_install_hooks", &ClientMsg::InstallHooks);
        snap(
            "host_hooks_installed",
            &HostMsg::HooksInstalled {
                ok: true,
                message: "hooks installed in /Users/x/.claude/settings.json".to_owned(),
            },
        );
    }

    /// Where a session runs: the directory it reports and the repository the host resolved it
    /// to (protocol 14). The canvas groups shells by that root, so it is wire-visible.
    #[test]
    fn session_place() {
        use slopty_proto::terminal::{SessionState, SessionSummary};
        snap(
            "host_session_opened",
            &HostMsg::SessionOpened(SessionSummary {
                id: session(),
                kind: slopty_proto::terminal::SessionKind::Terminal,
                title: "zsh".to_owned(),
                cwd: Some("/w/slopty/crates/ui".to_owned()),
                repo: Some("/w/slopty".to_owned()),
                cols: 80,
                rows: 24,
                state: SessionState::Running,
                viewers: 1,
                command: vec!["/bin/zsh".to_owned(), "-l".to_owned()],
            }),
        );
        snap(
            "host_term_cwd",
            &HostMsg::Term {
                session: session(),
                event: TermEvent::Cwd {
                    path: "/w/slopty/crates/ui".to_owned(),
                    repo: Some("/w/slopty".to_owned()),
                },
            },
        );
        // Outside any repository the root is absent, and the client falls back to the cwd.
        snap(
            "host_term_cwd_no_repo",
            &HostMsg::Term {
                session: session(),
                event: TermEvent::Cwd { path: "/tmp".to_owned(), repo: None },
            },
        );
    }

    /// File cards and the palette's quick open (protocol 20+): a read, a watch, a search
    /// and their answers, and a file item on the canvas.
    #[test]
    fn files() {
        snap("client_read_file", &ClientMsg::ReadFile { path: "/w/slopty/src/main.rs".to_owned() });
        snap(
            "client_find_files",
            &ClientMsg::FindFiles { root: "~".to_owned(), query: "main".to_owned() },
        );
        snap(
            "client_watch_files",
            &ClientMsg::WatchFiles {
                paths: vec!["/w/slopty/src/main.rs".to_owned(), "/w/notes.md".to_owned()],
            },
        );
        snap(
            "host_found_files",
            &HostMsg::FoundFiles {
                root: "~".to_owned(),
                query: "main".to_owned(),
                paths: vec!["w/slopty/src/main.rs".to_owned(), "w/manuals/".to_owned()],
            },
        );
        snap(
            "host_file",
            &HostMsg::File {
                path: "/w/slopty/src/main.rs".to_owned(),
                read: slopty_proto::file::FileRead::Text {
                    text: "fn main() {}\n// more".to_owned(),
                    more_lines: 3,
                    size: 4096,
                    modified_ms: 1_788_000_000_000,
                },
            },
        );
        snap(
            "host_file_missing",
            &HostMsg::File {
                path: "/w/gone".to_owned(),
                read: slopty_proto::file::FileRead::Missing { error: "No such file".to_owned() },
            },
        );
        snap(
            "host_canvas_file",
            &HostMsg::Canvas(slopty_proto::canvas::CanvasSync::Delta {
                version: 9,
                by: ClientId::from_uuid(Uuid::from_u128(0x42)),
                op: slopty_proto::canvas::CanvasOp::Upsert(slopty_proto::canvas::CanvasItem {
                    id: slopty_core::ItemId::from_uuid(Uuid::from_u128(0x77)),
                    kind: slopty_proto::canvas::ItemKind::File {
                        path: "/w/slopty/src/main.rs".to_owned(),
                    },
                    rect: slopty_proto::canvas::Rect { x: 10.0, y: 20.0, w: 520.0, h: 400.0 },
                    z: 3,
                    group: None,
                    sleeping: false,
                    name: None,
                }),
            }),
        );
        snap(
            "host_canvas_named",
            &HostMsg::Canvas(slopty_proto::canvas::CanvasSync::Delta {
                version: 10,
                by: ClientId::from_uuid(Uuid::from_u128(0x42)),
                op: slopty_proto::canvas::CanvasOp::Upsert(slopty_proto::canvas::CanvasItem {
                    id: slopty_core::ItemId::from_uuid(Uuid::from_u128(0x78)),
                    kind: slopty_proto::canvas::ItemKind::Terminal {
                        session: SessionId::from_uuid(Uuid::from_u128(0x79)),
                    },
                    rect: slopty_proto::canvas::Rect { x: 0.0, y: 0.0, w: 640.0, h: 400.0 },
                    z: 4,
                    group: Some("slopty".to_owned()),
                    sleeping: false,
                    name: Some("build box".to_owned()),
                }),
            }),
        );
    }

    #[test]
    fn screen_resize() {
        snap(
            "client_screen_resize",
            &ClientMsg::Screen(ScreenRequest::Resize {
                stream: StreamId(7),
                width: 1440,
                height: 900,
            }),
        );
    }

    #[test]
    fn receiver_report_and_rate() {
        snap(
            "client_screen_report",
            &ClientMsg::Screen(ScreenRequest::Report {
                stream: StreamId(7),
                report: ReceiverReport {
                    frames_ok: 30,
                    frames_fec: 1,
                    frames_lost: 2,
                    datagrams_lost: 9,
                    last_host_send_ts_us: 0x0102_0304,
                    hold_p50: slopty_core::Duration::from_millis(4),
                    hold_p95: slopty_core::Duration::from_millis(30),
                    owd_jitter: slopty_core::Duration::from_micros(700),
                    queue_depth: 1,
                    late_frames: 3,
                    acked_ltr: [0xabcd, 0, 0, 0],
                    acked_ltr_len: 1,
                    stalled_ms: 180,
                    stalls: 1,
                },
            }),
        );
        snap(
            "host_screen_source",
            &HostMsg::Screen(ScreenEvent::Source {
                stream: StreamId(7),
                state: slopty_proto::screen::SourceState::Idle,
            }),
        );
        snap(
            "host_screen_cursor",
            &HostMsg::Screen(ScreenEvent::Cursor {
                stream: StreamId(7),
                shape: Some(slopty_proto::screen::CursorShape {
                    w: 2,
                    h: 1,
                    hot_x: 1,
                    hot_y: 0,
                    bgra: vec![0, 0, 0, 255, 255, 255, 255, 128],
                    scale: 2,
                }),
            }),
        );
        snap(
            "host_screen_rate",
            &HostMsg::Screen(ScreenEvent::Rate {
                stream: StreamId(7),
                target_bps: 9_000_000,
                verdict: RateVerdict::Stall,
                capped: true,
            }),
        );
    }

    /// The datagram header is a fixed `zerocopy` layout, not postcard; a heartbeat is the
    /// header alone.
    #[test]
    fn media_heartbeat_header() {
        use slopty_proto::media::{Kind, MediaHeader};
        use zerocopy::IntoBytes as _;
        use zerocopy::little_endian::{U16, U32};
        let header = MediaHeader {
            stream: U32::new(7),
            frame: U32::new(0x0102_0304),
            index: U16::new(0),
            data_count: U16::new(0),
            parity_count: 0,
            kind: Kind::Heartbeat as u8,
            flags: 0,
            send_ms_lo: 0xab,
        };
        insta::assert_snapshot!("media_heartbeat", hex(header.as_bytes()));
    }

    #[test]
    fn clipboard() {
        snap(
            "client_clipboard",
            &ClientMsg::Screen(ScreenRequest::Clipboard { text: "fox".to_owned() }),
        );
        snap("host_clipboard", &HostMsg::Screen(ScreenEvent::Clipboard { text: "fox".to_owned() }));
        snap(
            "client_clipboard_image",
            &ClientMsg::Screen(ScreenRequest::ClipboardImage {
                media_type: "image/png".to_owned(),
                bytes: vec![0x89, b'P', b'N', b'G'],
            }),
        );
        snap("host_term_clipboard_write", &TermEvent::ClipboardWrite { text: "fox".to_owned() });
    }

    #[test]
    fn term_notification() {
        snap(
            "host_term_notification",
            &TermEvent::Notification { title: "Tests".to_owned(), body: "all green".to_owned() },
        );
    }

    #[test]
    fn lines_with_prompt_marks() {
        let mut prompt = Line::from_text("$ false", 8, Style::DEFAULT);
        prompt.mark = SemanticMark::Prompt { exit: Some(1), input: Some(2) };
        let mut cont = Line::from_text("> ", 8, Style::DEFAULT);
        cont.mark = SemanticMark::PromptContinuation { input: Some(2) };
        let mut output = Line::from_text("x", 8, Style::DEFAULT);
        output.mark = SemanticMark::Output;
        snap(
            "host_lines_marks",
            &TermEvent::Lines {
                start: slopty_grid::LineIndex(3),
                lines: vec![prompt, cont, output],
            },
        );
    }

    #[test]
    fn lines_with_links() {
        let mut line = Line::from_text("see https://a.b", 16, Style::DEFAULT);
        line.links.push(Hyperlink { col: 4, len: 11, uri: "https://a.b/".to_owned() });
        snap(
            "host_lines_links",
            &TermEvent::Lines { start: slopty_grid::LineIndex(40), lines: vec![line] },
        );
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
                images: vec![Placement {
                    image: 9,
                    generation: 4,
                    col: 2,
                    row: -1,
                    cols: 3,
                    rows: 2,
                    x_offset: 1,
                    y_offset: 0,
                    width: 24,
                    height: 32,
                    source: PixelRect { x: 0, y: 0, width: 6, height: 8 },
                    z: -1,
                }],
            }),
        );
    }

    #[test]
    fn term_image() {
        snap(
            "host_term_image",
            &TermEvent::Image {
                id: 9,
                generation: 4,
                width: 2,
                height: 1,
                rgba: vec![255, 0, 0, 255, 0, 0, 255, 128],
            },
        );
    }
}

#[cfg(test)]
mod size_report {
    use slopty_grid::{Cursor, CursorShape, Line, RowUpdate, Style, TermModes};
    use slopty_proto::codec;
    use slopty_proto::terminal::{Frame, TermEvent};

    /// Encoded size of a full, link-free frame; `cargo nextest run -p slopty-proto size_report
    /// --no-capture` prints it (the numbers live in `docs/MEASUREMENTS.md`).
    #[test]
    fn size_report() {
        for (cols, rows) in [(80_u16, 24_u16), (200, 60)] {
            let blank: Vec<RowUpdate> =
                (0..rows).map(|row| RowUpdate { row, line: Line::blank(cols) }).collect();
            let text: Vec<RowUpdate> = (0..rows)
                .map(|row| RowUpdate {
                    row,
                    line: Line::from_text(&"x".repeat(usize::from(cols)), cols, Style::DEFAULT),
                })
                .collect();
            // Every row a prompt with a status: the worst case for the per-row mark.
            let prompts: Vec<RowUpdate> = (0..rows)
                .map(|row| {
                    let mut line = Line::blank(cols);
                    line.mark = slopty_grid::SemanticMark::Prompt { exit: Some(1), input: Some(2) };
                    RowUpdate { row, line }
                })
                .collect();
            for (name, updates) in [("blank", blank), ("text", text), ("prompts", prompts)] {
                let frame = Frame {
                    seq: 1,
                    full: true,
                    epoch: 0,
                    cols,
                    rows,
                    cursor: Cursor {
                        row: 0,
                        col: 0,
                        shape: CursorShape::Block,
                        visible: true,
                        blink: true,
                    },
                    modes: TermModes::empty(),
                    oldest_line: slopty_grid::LineIndex(0),
                    first_visible_line: slopty_grid::LineIndex(0),
                    total_lines: u64::from(rows),
                    input_ack: 0,
                    updates,
                    images: Vec::new(),
                };
                let bytes = codec::encode(&TermEvent::Frame(frame)).expect("encodes");
                eprintln!("frame {cols}x{rows} {name}: {} bytes", bytes.len());
            }
        }
    }
}
