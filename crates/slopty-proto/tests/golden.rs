//! Golden byte snapshots of representative messages. A changed snapshot is a protocol change:
//! bump [`slopty_proto::PROTOCOL_VERSION`] and accept it deliberately (`cargo insta review`).

#[cfg(test)]
mod golden {
    use slopty_core::{ClientId, MonoTime, SessionId, StreamId, XferId};
    use slopty_grid::{
        Cursor, CursorShape, Hyperlink, Line, RowUpdate, SemanticMark, Style, TermModes,
    };
    use slopty_proto::handshake::{Caps, ClientKind, Hello};
    use slopty_proto::input::{KeyAction, KeyCode, KeyEvent, Mods};
    use slopty_proto::orchestration::Port;
    use slopty_proto::screen::{Feedback, RateVerdict, ReceiverReport, ScreenEvent, ScreenRequest};
    use slopty_proto::terminal::{
        ColorOverrides, Frame, PixelRect, Placement, SearchMatch, TermColors, TermEvent,
        TermRequest,
    };
    use slopty_proto::transfer::{
        BulkHeader, ClipItem, ClipMsg, Dest, Offer, Peer, Purpose, TunnelOpen, UniHead, XferMsg,
    };
    use slopty_proto::{ClientMsg, PROTOCOL_VERSION, WorkerMsg, codec};
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
    fn point_and_pointed() {
        let item = slopty_core::ItemId::from_uuid(Uuid::from_u128(0x77));
        snap("client_point", &ClientMsg::Point { item });
        snap(
            "worker_pointed",
            &WorkerMsg::Items(slopty_proto::items::ItemSync::Pointed {
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
            "worker_term_colors",
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
        snap("worker_pong", &WorkerMsg::Pong { sent_at: MonoTime::from_nanos(1_000_000) });
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
            "worker_search_invalid",
            &TermEvent::SearchInvalid {
                needle: "(".to_owned(),
                message: "unclosed group".to_owned(),
            },
        );
        snap(
            "worker_matches",
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
        let refresh =
            Feedback::Refresh { stream: StreamId(7), last_good_frame: 300, keyframe: true };
        let bytes = codec::encode_body(&refresh).expect("encodes");
        insta::assert_snapshot!("client_refresh", hex(&bytes));
    }

    #[test]
    fn agent() {
        use slopty_proto::agent::{AgentEvent, AgentKind, AgentSource, AgentStatus, BlockReason};
        snap(
            "worker_agent_hook",
            &WorkerMsg::Agent(AgentEvent {
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
            "worker_agent_process",
            &WorkerMsg::Agent(AgentEvent {
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
            "worker_hooks_installed",
            &WorkerMsg::HooksInstalled {
                ok: true,
                message: "hooks installed in /Users/x/.claude/settings.json".to_owned(),
            },
        );
    }

    /// Where a session runs: the directory it reports and the repository the worker resolved it
    /// to (protocol 14). The canvas groups shells by that root, so it is wire-visible.
    #[test]
    fn session_place() {
        use slopty_proto::terminal::{SessionState, SessionSummary};
        snap(
            "worker_session_opened",
            &WorkerMsg::SessionOpened(SessionSummary {
                id: session(),
                title: "zsh".to_owned(),
                cwd: Some("/w/slopty/crates/ui".to_owned()),
                repo: Some("/w/slopty".to_owned()),
                cols: 80,
                rows: 24,
                state: SessionState::Running,
                viewers: 1,
                command: vec!["/bin/zsh".to_owned(), "-l".to_owned()],
                agent: Some(slopty_proto::agent::SessionAgent {
                    kind: slopty_proto::agent::AgentKind::ClaudeCode,
                    status: slopty_proto::agent::AgentStatus::Working,
                    source: slopty_proto::agent::AgentSource::Hook,
                }),
            }),
        );
        snap(
            "worker_term_cwd",
            &WorkerMsg::Term {
                session: session(),
                event: TermEvent::Cwd {
                    path: "/w/slopty/crates/ui".to_owned(),
                    repo: Some("/w/slopty".to_owned()),
                },
            },
        );
        // Outside any repository the root is absent, and the client falls back to the cwd.
        snap(
            "worker_term_cwd_no_repo",
            &WorkerMsg::Term {
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
            "worker_found_files",
            &WorkerMsg::FoundFiles {
                root: "~".to_owned(),
                query: "main".to_owned(),
                paths: vec!["w/slopty/src/main.rs".to_owned(), "w/manuals/".to_owned()],
            },
        );
        snap(
            "worker_file",
            &WorkerMsg::File {
                path: "/w/slopty/src/main.rs".to_owned(),
                read: slopty_proto::file::FileRead::Text {
                    text: "fn main() {}\n// more".to_owned(),
                    more_lines: 3,
                    size: 4096,
                    modified_ms: 1_788_000_000_000,
                    final_newline: false,
                },
            },
        );
        snap(
            "worker_file_missing",
            &WorkerMsg::File {
                path: "/w/gone".to_owned(),
                read: slopty_proto::file::FileRead::Missing { error: "No such file".to_owned() },
            },
        );
        snap(
            "client_write_file",
            &ClientMsg::WriteFile {
                path: "/w/slopty/notes.md".to_owned(),
                text: "# Notes\n".to_owned(),
                base_modified_ms: Some(1_700_000_000_000),
            },
        );
        snap(
            "worker_written_conflict",
            &WorkerMsg::Written {
                path: "/w/slopty/notes.md".to_owned(),
                result: slopty_proto::file::WriteResult::Conflict {
                    modified_ms: 1_700_000_000_500,
                },
            },
        );
        snap(
            "worker_item_browser",
            &WorkerMsg::Items(slopty_proto::items::ItemSync::Delta {
                version: 10,
                by: ClientId::from_uuid(Uuid::from_u128(0x42)),
                op: slopty_proto::items::ItemOp::Upsert(slopty_proto::items::Item {
                    id: slopty_core::ItemId::from_uuid(Uuid::from_u128(0x77)),
                    kind: slopty_proto::items::ItemKind::Browser {
                        url: "http://localhost:5173/".to_owned(),
                    },
                    sleeping: false,
                    name: None,
                }),
            }),
        );
        snap(
            "worker_item_file",
            &WorkerMsg::Items(slopty_proto::items::ItemSync::Delta {
                version: 9,
                by: ClientId::from_uuid(Uuid::from_u128(0x42)),
                op: slopty_proto::items::ItemOp::Upsert(slopty_proto::items::Item {
                    id: slopty_core::ItemId::from_uuid(Uuid::from_u128(0x77)),
                    kind: slopty_proto::items::ItemKind::File {
                        path: "/w/slopty/src/main.rs".to_owned(),
                    },
                    sleeping: false,
                    name: None,
                }),
            }),
        );
        snap(
            "worker_item_named",
            &WorkerMsg::Items(slopty_proto::items::ItemSync::Delta {
                version: 10,
                by: ClientId::from_uuid(Uuid::from_u128(0x42)),
                op: slopty_proto::items::ItemOp::Upsert(slopty_proto::items::Item {
                    id: slopty_core::ItemId::from_uuid(Uuid::from_u128(0x78)),
                    kind: slopty_proto::items::ItemKind::Terminal {
                        session: SessionId::from_uuid(Uuid::from_u128(0x79)),
                    },
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
                    last_worker_send_ts_us: 0x0102_0304,
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
            "worker_screen_source",
            &WorkerMsg::Screen(ScreenEvent::Source {
                stream: StreamId(7),
                state: slopty_proto::screen::SourceState::Idle,
            }),
        );
        snap(
            "worker_screen_cursor",
            &WorkerMsg::Screen(ScreenEvent::Cursor {
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
            "worker_screen_rate",
            &WorkerMsg::Screen(ScreenEvent::Rate {
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
        let origin = Peer::Client(ClientId::from_uuid(Uuid::from_u128(7)));
        snap(
            "client_clip_offer",
            &ClientMsg::Clip(ClipMsg::Offer(Offer {
                origin,
                generation: 3,
                items: vec![
                    ClipItem {
                        uti: "public.utf8-plain-text".to_owned(),
                        size: 3,
                        hash: [1; 32],
                        inline: Some(b"fox".to_vec()),
                    },
                    ClipItem {
                        uti: "public.png".to_owned(),
                        size: 1 << 20,
                        hash: [2; 32],
                        inline: None,
                    },
                ],
            })),
        );
        snap("client_clip_watch", &ClientMsg::Clip(ClipMsg::Watch(true)));
        snap(
            "worker_clip_fetch",
            &WorkerMsg::Clip(ClipMsg::Fetch { generation: 3, uti: "public.png".to_owned() }),
        );
        snap(
            "worker_clip_data",
            &WorkerMsg::Clip(ClipMsg::Data {
                generation: 3,
                uti: "public.html".to_owned(),
                bytes: b"<b>fox</b>".to_vec(),
            }),
        );
        snap("worker_term_clipboard_write", &TermEvent::ClipboardWrite { text: "fox".to_owned() });
    }

    #[test]
    fn transfers() {
        let xfer = XferId::from_uuid(Uuid::from_u128(9));
        let session = SessionId::from_uuid(Uuid::from_u128(1));
        snap(
            "client_xfer_begin",
            &ClientMsg::Xfer(XferMsg::Begin {
                xfer,
                dest: Some(Dest::SessionCwd(session)),
                files: 2,
                bytes: 4096,
            }),
        );
        snap(
            "worker_xfer_done",
            &WorkerMsg::Xfer(XferMsg::Done {
                xfer,
                name: "src/a.rs".to_owned(),
                path: "/Users/c/p/src/a.rs".to_owned(),
                hash: [3; 32],
            }),
        );
        snap(
            "client_xfer_fetch_resumed",
            &ClientMsg::Xfer(XferMsg::Fetch {
                xfer,
                path: "~/project/out".to_owned(),
                held: vec![("out/big.bin".to_owned(), 1 << 20)],
            }),
        );
        snap(
            "worker_xfer_finished",
            &WorkerMsg::Xfer(XferMsg::Finished { xfer, paths: vec!["/Users/c/p/src".to_owned()] }),
        );
        snap(
            "uni_bulk",
            &UniHead::Bulk(BulkHeader {
                xfer,
                purpose: Purpose::Upload,
                name: "src/a.rs".to_owned(),
                size: 4096,
                mtime_ms: 1_700_000_000_000,
                mode: 0o644,
                offset: 1024,
            }),
        );
        snap("uni_session", &UniHead::Session { session });
        snap("tunnel_open", &TunnelOpen { port: 5173 });
        snap(
            "worker_ports",
            &WorkerMsg::Ports {
                session,
                ports: vec![Port {
                    number: 5173,
                    pid: 4242,
                    process: "node".to_owned(),
                    session: Some(session),
                }],
            },
        );
    }

    #[test]
    fn term_notification() {
        snap(
            "worker_term_notification",
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
            "worker_lines_marks",
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
            "worker_lines_links",
            &TermEvent::Lines { start: slopty_grid::LineIndex(40), lines: vec![line] },
        );
    }

    #[test]
    fn frame() {
        let mut line = Line::from_text("$ ls", 8, Style::DEFAULT);
        line.cells[1].style.flags = slopty_grid::StyleFlags::BOLD;
        snap(
            "worker_frame",
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
    fn markers() {
        snap("worker_term_marker", &TermEvent::Marker { id: 41 });
        snap(
            "client_term_reached",
            &ClientMsg::Term { session: session(), req: TermRequest::Reached { marker: 41 } },
        );
    }

    #[test]
    fn term_image() {
        snap(
            "worker_term_image",
            &TermEvent::Image {
                id: 9,
                generation: 4,
                width: 2,
                height: 1,
                rgba: vec![255, 0, 0, 255, 0, 0, 255, 128],
            },
        );
    }

    #[test]
    fn server_links() {
        use slopty_core::WorkerId;
        use slopty_proto::orchestration::{Line, Outcome, TermRef, Verb};
        use slopty_proto::screen::VideoCodec;
        use slopty_proto::server::{
            DisplayCap, FromServer, Liveness, Os, Registration, Role, ToServer, WorkerCaps,
            WorkerInfo,
        };

        let worker = WorkerId::from_uuid(Uuid::from_u128(0x77));
        let caps = WorkerCaps {
            os: Os::MacOs,
            os_version: "26.5".to_owned(),
            arch: "aarch64".to_owned(),
            cpus: 24,
            memory: 128 << 30,
            encoders: vec![VideoCodec::Hevc, VideoCodec::H264],
            displays: vec![DisplayCap { id: 1, w: 2560.0, h: 1440.0, scale: 2.0, hz: 120.0 }],
            agents: Vec::new(),
            can_capture: true,
            can_inject: true,
            load: 1.5,
            version: "0.1.0".to_owned(),
        };
        snap(
            "server_worker_hello",
            &ToServer::Hello {
                protocol: PROTOCOL_VERSION,
                role: Role::Worker(Registration {
                    worker,
                    name: "mac-studio".to_owned(),
                    port: 45570,
                    caps: caps.clone(),
                    sessions: Vec::new(),
                }),
            },
        );
        let term = TermRef { worker, session: session() };
        snap(
            "server_request",
            &FromServer::Request {
                id: 7,
                verb: Verb::ReadOutput { term, since: Some(1200), max_lines: 200 },
            },
        );
        snap(
            "server_reply",
            &ToServer::Reply {
                id: 7,
                outcome: Outcome::Output {
                    lines: vec![Line { index: 1200, text: "cargo build".to_owned() }],
                    next: 1201,
                },
            },
        );
        snap(
            "server_directory",
            &FromServer::Worker(WorkerInfo {
                worker,
                name: "mac-studio".to_owned(),
                address: "100.64.0.7:45570".to_owned(),
                liveness: Liveness::Online,
                caps,
                last_seen_ms: 1_790_000_000_000,
            }),
        );
    }
}

#[cfg(test)]
mod orchestration {
    use slopty_core::{SessionId, WorkerId};
    use slopty_proto::agent::{AgentKind, AgentStatus, BlockReason};
    use slopty_proto::codec;
    use slopty_proto::orchestration::{
        DirEntry, EventFilter, FileKind, FileStat, Happening, HubEvent, Outcome, Size, TermRef,
        Verb,
    };
    use slopty_proto::server::{FromServer, ToServer};
    use uuid::Uuid;

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

    fn term() -> TermRef {
        TermRef {
            worker: WorkerId::from_uuid(Uuid::from_u128(0x77)),
            session: SessionId::from_uuid(Uuid::from_u128(
                0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10,
            )),
        }
    }

    fn request(verb: Verb) -> FromServer {
        FromServer::Request { id: 8, verb }
    }

    fn reply(outcome: Outcome) -> ToServer {
        ToServer::Reply { id: 8, outcome }
    }

    #[test]
    fn sizes_ranges_and_directories() {
        let worker = term().worker;
        let size = Some(Size { cols: 200, rows: 50 });
        snap(
            "server_request_spawn",
            &request(Verb::SpawnAgent {
                worker,
                agent: AgentKind::ClaudeCode,
                cwd: "~/src/app".to_owned(),
                prompt: Some("fix it".to_owned()),
                args: vec!["--model".to_owned(), "opus".to_owned()],
                env: vec![("A".to_owned(), "1".to_owned())],
                size,
            }),
        );
        let resize = Verb::ResizeTerminal { term: term(), size: Size { cols: 100, rows: 30 } };
        snap("server_request_resize", &request(resize));
        let read =
            Verb::ReadFile { worker, path: "/tmp/a".to_owned(), offset: 4096, length: Some(1024) };
        snap("server_request_read_range", &request(read));
        let file = Outcome::File { bytes: b"hi".to_vec(), offset: 4096, size: 8192 };
        snap("server_reply_file", &reply(file));
        snap(
            "server_request_list_dir",
            &request(Verb::ListDir { worker, path: "~".to_owned(), max: 1000 }),
        );
        let entry = DirEntry {
            name: "src".to_owned(),
            kind: FileKind::Dir,
            size: 96,
            modified_ms: 1_790_000_000_000,
        };
        snap("server_reply_dir", &reply(Outcome::Dir { entries: vec![entry], total: 2 }));
        let stat = FileStat {
            kind: FileKind::File,
            size: 12,
            modified_ms: 1_790_000_000_000,
            mode: 0o644,
        };
        snap("server_reply_stat", &reply(Outcome::Stat(Some(stat))));
    }

    #[test]
    fn events_and_forgetting() {
        let asked = ToServer::Request {
            id: 9,
            verb: Verb::Events {
                since: Some(41),
                timeout_ms: 60_000,
                filter: EventFilter::AgentNeedsInput,
            },
        };
        snap("server_request_events", &asked);
        let event = HubEvent {
            seq: 41,
            at_ms: 1_790_000_000_000,
            what: Happening::Agent {
                term: term(),
                kind: AgentKind::ClaudeCode,
                status: AgentStatus::Blocked(BlockReason::Question),
                detail: Some("Which branch?".to_owned()),
            },
        };
        let answer = Outcome::Events { events: vec![event], next: 42, missed: 0 };
        snap("server_reply_events", &FromServer::Reply { id: 9, outcome: answer });
        let forget =
            ToServer::Request { id: 10, verb: Verb::ForgetWorker { worker: term().worker } };
        snap("server_request_forget", &forget);
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
