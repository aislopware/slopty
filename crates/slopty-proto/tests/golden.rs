//! Golden byte snapshots of representative messages. A changed snapshot is a wire change: accept
//! it deliberately (`cargo insta review`).

#[cfg(test)]
mod golden {
    use slopty_core::{ClientId, MonoTime, SessionId, StreamId, WallMs, XferId};
    use slopty_grid::{
        Cursor, CursorShape, Hyperlink, Line, RowUpdate, SemanticMark, Style, TermModes,
    };
    use slopty_proto::datagram::{ClientDatagram, term_datagram};
    use slopty_proto::handshake::Hello;
    use slopty_proto::input::{KeyAction, KeyCode, KeyEvent, Mods, MouseButton};
    use slopty_proto::orchestration::Port;
    use slopty_proto::screen::{
        Feedback, RateVerdict, ReceiverReport, ScreenEvent, ScreenInput, ScreenRequest,
    };
    use slopty_proto::terminal::{
        ColorOverrides, Frame, PixelRect, Placement, SearchMatch, TermColors, TermEvent,
        TermRequest,
    };
    use slopty_proto::transfer::{
        BulkHeader, ClipFormat, ClipItem, ClipMsg, Dest, Offer, Peer, Purpose, TunnelOpen, UniHead,
        XferMsg,
    };
    use slopty_proto::{ClientMsg, WorkerMsg, codec};
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
                client: ClientId::from_uuid(Uuid::from_u128(0x42)),
                name: "iPad".to_owned(),
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
        snap("worker_path_direct", &WorkerMsg::Path(slopty_proto::tailnet::LinkPath::Direct));
        snap(
            "worker_path_derp",
            &WorkerMsg::Path(slopty_proto::tailnet::LinkPath::Derp { region: "fra".to_owned() }),
        );
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
        let bytes = ClientDatagram::Feedback(nack).encode().expect("encodes");
        insta::assert_snapshot!("client_nack", hex(&bytes));
        let refresh =
            Feedback::Refresh { stream: StreamId(7), last_good_frame: 300, keyframe: true };
        let bytes = ClientDatagram::Feedback(refresh).encode().expect("encodes");
        insta::assert_snapshot!("client_refresh", hex(&bytes));
    }

    /// A keystroke's copy: the session, its place among the session's inputs, the request.
    #[test]
    fn input_copy() {
        let copy = ClientDatagram::Input {
            session: session(),
            seq: 12,
            req: TermRequest::Raw(b"x".to_vec()),
        };
        let bytes = copy.encode().expect("encodes");
        insta::assert_snapshot!("client_input_copy", hex(&bytes));
    }

    /// A window stream's input copy: the stream, its place among the stream's numbered
    /// requests, how many of those apply only in order, the input.
    #[test]
    fn screen_input_copy() {
        let copy = ClientDatagram::ScreenInput {
            stream: StreamId(7),
            seq: 12,
            ordered: 3,
            input: ScreenInput::Button {
                button: MouseButton::Left,
                down: true,
                x: 10.5,
                y: 20.0,
                clicks: 1,
                mods: Mods::SUPER,
            },
        };
        let bytes = copy.encode().expect("encodes");
        insta::assert_snapshot!("client_screen_input_copy", hex(&bytes));
    }

    /// An echo's copy: the `Term` channel byte, then the session and the event.
    #[test]
    fn frame_copy() {
        let event = TermEvent::Frame(Frame {
            seq: 3,
            full: false,
            epoch: 1,
            cols: 8,
            rows: 2,
            cursor: Cursor {
                row: 0,
                col: 2,
                shape: CursorShape::Block,
                visible: true,
                blink: false,
            },
            modes: TermModes::empty(),
            oldest_line: slopty_grid::LineIndex(0),
            first_visible_line: slopty_grid::LineIndex(0),
            total_lines: 2,
            input_ack: 0,
            updates: vec![RowUpdate {
                row: 0,
                line: Line::from_text("$ x", 8, Style::DEFAULT).into(),
            }],
            images: Vec::new(),
        });
        let body = codec::encode_body(&event).expect("encodes");
        let bytes = term_datagram(session(), &body).expect("encodes");
        insta::assert_snapshot!("worker_frame_copy", hex(&bytes));
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
                since_ms: WallMs::from_millis(1_790_000_060_000),
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
                since_ms: WallMs::from_millis(1_790_000_060_000),
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

    /// The worker's greeting names its home, so a client writes `~` knowing, and what it can do;
    /// a later change to what it can do comes on its own.
    #[test]
    fn worker_greeting() {
        use slopty_core::WorkerId;
        use slopty_proto::handshake::HelloAck;
        use slopty_proto::server::{Os, WorkerCaps};
        let caps = WorkerCaps {
            os: Os::MacOs,
            os_version: "26.5".to_owned(),
            arch: "aarch64".to_owned(),
            cpus: 12,
            memory: 32 << 30,
            encoders: Vec::new(),
            displays: Vec::new(),
            agents: Vec::new(),
            can_capture: false,
            can_inject: true,
            virtual_displays: false,
            version: "0.1.0".to_owned(),
            lan: Vec::new(),
            wake_on_lan: None,
        };
        snap(
            "worker_hello_ack",
            &WorkerMsg::HelloAck(HelloAck {
                worker: WorkerId::from_uuid(Uuid::from_u128(0x77)),
                name: "mac-studio".to_owned(),
                home: "/Users/w".to_owned(),
                caps: caps.clone(),
                load: 2.5,
                sessions: Vec::new(),
            }),
        );
        snap(
            "worker_caps",
            &WorkerMsg::Caps(WorkerCaps { can_capture: true, virtual_displays: true, ..caps }),
        );
        snap("worker_load", &WorkerMsg::Load(3.25));
    }

    /// A session asked for under the client's number, and the typed failure that answers it
    /// when it cannot be opened; a session's own failure is typed too.
    #[test]
    fn open_session_and_failures() {
        use slopty_proto::orchestration::ErrorCode;
        use slopty_proto::terminal::{OpenSession, TermError, TermSize};
        snap(
            "client_open_session",
            &ClientMsg::OpenSession {
                request: 5,
                spec: OpenSession {
                    size: TermSize::default(),
                    cwd: Some("/w".to_owned()),
                    command: Vec::new(),
                    env: vec![("A".to_owned(), "1".to_owned())],
                    title: None,
                    attach: true,
                },
            },
        );
        snap(
            "worker_failed",
            &WorkerMsg::Failed {
                request: 5,
                code: ErrorCode::Failed,
                message: "No such directory: /w".to_owned(),
            },
        );
        snap(
            "worker_term_error_input_full",
            &WorkerMsg::Term { session: session(), event: TermEvent::Error(TermError::InputFull) },
        );
        snap(
            "worker_term_error_engine",
            &TermEvent::Error(TermError::Engine("lines evicted".to_owned())),
        );
    }

    /// Where a session runs: the directory it reports, the repository the worker resolved it
    /// to and the branch checked out there, and when it started. The workspace groups shells
    /// by that root and names them by the branch, so both are wire-visible.
    #[test]
    fn session_place() {
        use slopty_proto::terminal::{SessionState, SessionSummary};
        let summary = SessionSummary {
            id: session(),
            title: "zsh".to_owned(),
            cwd: Some("/w/slopty/crates/ui".to_owned()),
            repo: Some("/w/slopty".to_owned()),
            branch: Some("main".to_owned()),
            changes: Some(slopty_proto::terminal::RepoChanges { files: 3, added: 12, removed: 4 }),
            started_ms: WallMs::from_millis(1_790_000_000_000),
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 1,
            command: vec!["/bin/zsh".to_owned(), "-l".to_owned()],
            agent: Some(slopty_proto::agent::SessionAgent {
                kind: slopty_proto::agent::AgentKind::ClaudeCode,
                status: slopty_proto::agent::AgentStatus::Working,
                source: slopty_proto::agent::AgentSource::Hook,
                since_ms: WallMs::from_millis(1_790_000_060_000),
            }),
        };
        snap(
            "worker_session_opened",
            &WorkerMsg::SessionOpened { request: 5, summary: summary.clone() },
        );
        snap("worker_session_changed", &WorkerMsg::SessionChanged(summary));
        snap(
            "worker_term_cwd",
            &WorkerMsg::Term {
                session: session(),
                event: TermEvent::Cwd {
                    path: "/w/slopty/crates/ui".to_owned(),
                    repo: Some("/w/slopty".to_owned()),
                    branch: Some("feature/rows".to_owned()),
                },
            },
        );
        // Outside any repository the root is absent, and the client falls back to the cwd.
        snap(
            "worker_term_cwd_no_repo",
            &WorkerMsg::Term {
                session: session(),
                event: TermEvent::Cwd { path: "/tmp".to_owned(), repo: None, branch: None },
            },
        );
    }

    /// File tiles and the palette's quick open (protocol 20+): a read, a watch, a search
    /// and their answers, and a file item in the registry.
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
                    size: 4096,
                    modified_ms: WallMs::from_millis(1_788_000_000_000),
                    final_newline: false,
                },
            },
        );
        snap(
            "worker_file_streamed",
            &WorkerMsg::File {
                path: "/w/slopty/src/big.rs".to_owned(),
                read: slopty_proto::file::FileRead::Streamed {
                    xfer: XferId::from_uuid(Uuid::from_u128(0x0f11e)),
                    size: 1_400_000,
                    modified_ms: WallMs::from_millis(1_788_000_000_000),
                    final_newline: true,
                },
            },
        );
        snap(
            "worker_file_too_large",
            &WorkerMsg::File {
                path: "/w/logs/huge.log".to_owned(),
                read: slopty_proto::file::FileRead::TooLarge { size: 40 << 20 },
            },
        );
        snap(
            "uni_bulk_file_text",
            &UniHead::Bulk(BulkHeader {
                xfer: XferId::from_uuid(Uuid::from_u128(0x0f11e)),
                purpose: Purpose::FileText,
                name: String::new(),
                size: 1_399_999,
                mtime_ms: WallMs::from_millis(1_788_000_000_000),
                mode: 0,
                offset: 0,
            }),
        );
        snap(
            "uni_bulk_save",
            &UniHead::Bulk(BulkHeader {
                xfer: XferId::from_uuid(Uuid::from_u128(0x5a7e)),
                purpose: Purpose::Save {
                    path: "/w/slopty/src/big.rs".to_owned(),
                    base_modified_ms: Some(WallMs::from_millis(1_788_000_000_000)),
                },
                name: String::new(),
                size: 1_400_001,
                mtime_ms: WallMs::ZERO,
                mode: 0,
                offset: 0,
            }),
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
                base_modified_ms: Some(WallMs::from_millis(1_700_000_000_000)),
            },
        );
        snap(
            "worker_written_conflict",
            &WorkerMsg::Written {
                path: "/w/slopty/notes.md".to_owned(),
                result: slopty_proto::file::WriteResult::Conflict {
                    modified_ms: WallMs::from_millis(1_700_000_000_500),
                },
            },
        );
        snap(
            "worker_item_browser",
            &WorkerMsg::Items(slopty_proto::items::ItemSync::Delta {
                version: 10,
                by: ClientId::from_uuid(Uuid::from_u128(0x42)),
                op: slopty_proto::items::ItemOp::Add(slopty_proto::items::Item {
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
                op: slopty_proto::items::ItemOp::Add(slopty_proto::items::Item {
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
                op: slopty_proto::items::ItemOp::Add(slopty_proto::items::Item {
                    id: slopty_core::ItemId::from_uuid(Uuid::from_u128(0x78)),
                    kind: slopty_proto::items::ItemKind::Terminal {
                        session: SessionId::from_uuid(Uuid::from_u128(0x79)),
                    },
                    sleeping: false,
                    name: Some("build box".to_owned()),
                }),
            }),
        );
        snap(
            "worker_item_renamed",
            &WorkerMsg::Items(slopty_proto::items::ItemSync::Delta {
                version: 11,
                by: ClientId::from_uuid(Uuid::from_u128(0x42)),
                op: slopty_proto::items::ItemOp::Rename {
                    id: slopty_core::ItemId::from_uuid(Uuid::from_u128(0x78)),
                    name: Some("api".to_owned()),
                },
            }),
        );
        snap(
            "client_item_set_note",
            &ClientMsg::Items(slopty_proto::items::ItemOp::SetNote {
                id: slopty_core::ItemId::from_uuid(Uuid::from_u128(0x7a)),
                text: "# Plan\n".to_owned(),
            }),
        );
        snap(
            "client_item_set_url",
            &ClientMsg::Items(slopty_proto::items::ItemOp::SetUrl {
                id: slopty_core::ItemId::from_uuid(Uuid::from_u128(0x7b)),
                url: "http://localhost:3000/docs".to_owned(),
            }),
        );
    }

    /// Folder tiles: the item, a move to another directory, the listing asked for and its
    /// three answers.
    #[test]
    fn folders() {
        use slopty_proto::folder::{FolderEntry, Listing};
        use slopty_proto::items::{Item, ItemKind, ItemOp, ItemSync};
        use slopty_proto::orchestration::FileKind;

        let id = slopty_core::ItemId::from_uuid(Uuid::from_u128(0x7c));
        snap(
            "worker_item_folder",
            &WorkerMsg::Items(ItemSync::Delta {
                version: 12,
                by: ClientId::from_uuid(Uuid::from_u128(0x42)),
                op: ItemOp::Add(Item {
                    id,
                    kind: ItemKind::Folder { path: "/w/slopty".to_owned() },
                    sleeping: false,
                    name: None,
                }),
            }),
        );
        snap(
            "client_item_set_folder",
            &ClientMsg::Items(ItemOp::SetFolder { id, path: "/w/slopty/src".to_owned() }),
        );
        snap("client_list_folder", &ClientMsg::ListFolder { path: "~/src".to_owned() });
        snap(
            "worker_folder_listed",
            &WorkerMsg::Folder {
                path: "~/src".to_owned(),
                listing: Listing::Listed {
                    dir: "/Users/w/src".to_owned(),
                    entries: vec![
                        FolderEntry {
                            name: ".git".to_owned(),
                            kind: FileKind::Dir,
                            link: false,
                            hidden: true,
                            size: 0,
                            items: Some(12),
                            modified_ms: WallMs::from_millis(1_788_000_000_000),
                        },
                        FolderEntry {
                            name: "main.rs".to_owned(),
                            kind: FileKind::File,
                            link: true,
                            hidden: false,
                            size: 4096,
                            items: None,
                            modified_ms: WallMs::from_millis(1_788_000_000_500),
                        },
                    ],
                    total: 3,
                },
            },
        );
        snap(
            "worker_folder_not_folder",
            &WorkerMsg::Folder { path: "/w/a.txt".to_owned(), listing: Listing::NotFolder },
        );
        snap(
            "worker_folder_missing",
            &WorkerMsg::Folder {
                path: "/w/gone".to_owned(),
                listing: Listing::Missing { error: "No such file or directory".to_owned() },
            },
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
                scale: None,
            }),
        );
        snap(
            "client_screen_resize_scale",
            &ClientMsg::Screen(ScreenRequest::Resize {
                stream: StreamId(7),
                width: 2752,
                height: 2064,
                scale: Some(2.0),
            }),
        );
    }

    /// A quality that asks for colour at every pixel.
    #[test]
    fn screen_quality_full_chroma() {
        use slopty_proto::screen::{Chroma, Quality, VideoCodec};
        snap(
            "client_screen_set_quality_full_chroma",
            &ClientMsg::Screen(ScreenRequest::SetQuality {
                stream: StreamId(7),
                quality: Quality {
                    fps: 60,
                    bitrate_bps: 30_000_000,
                    scale: 1.0,
                    codec: VideoCodec::Hevc,
                    chroma: Chroma::Full,
                },
            }),
        );
    }

    /// A display made for the client: the ask, then what the worker made of it, typed.
    #[test]
    fn screen_display() {
        use slopty_core::DisplayId;
        use slopty_proto::screen::{
            DisplayKey, DisplayShape, NoVirtualDisplay, Quality, VirtualDisplay,
        };
        let key = DisplayKey(*b"slopty-ipad-pro!");
        snap(
            "client_screen_open_display",
            &ClientMsg::Screen(ScreenRequest::OpenDisplay {
                key,
                shape: DisplayShape { width: 2752, height: 2064, scale: 2.0, refresh_hz: 120 },
                quality: Quality::default(),
            }),
        );
        snap(
            "worker_screen_display_made",
            &WorkerMsg::Screen(ScreenEvent::Display {
                stream: StreamId(7),
                key,
                display: VirtualDisplay::Made(DisplayId(0x8000_0003)),
            }),
        );
        snap(
            "worker_screen_display_physical",
            &WorkerMsg::Screen(ScreenEvent::Display {
                stream: StreamId(7),
                key,
                display: VirtualDisplay::Physical {
                    display: DisplayId(1),
                    why: NoVirtualDisplay::Unlisted,
                },
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
            channel: slopty_proto::datagram::Channel::Media as u8,
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
                        format: ClipFormat::Text,
                        size: 3,
                        hash: [1; 32],
                        inline: Some(b"fox".to_vec()),
                    },
                    ClipItem {
                        format: ClipFormat::Png,
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
            &WorkerMsg::Clip(ClipMsg::Fetch { generation: 3, format: ClipFormat::Png }),
        );
        snap(
            "worker_clip_data",
            &WorkerMsg::Clip(ClipMsg::Data {
                generation: 3,
                format: ClipFormat::Html,
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
            "client_xfer_begin_attachment",
            &ClientMsg::Xfer(XferMsg::Begin {
                xfer,
                dest: Some(Dest::Attachment),
                files: 1,
                bytes: 2048,
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
                mtime_ms: WallMs::from_millis(1_700_000_000_000),
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
                updates: vec![RowUpdate { row: 0, line: line.into() }],
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

    /// ⌘V and ⌃V with a picture on the client's clipboard: the offer is placed on the
    /// worker's pasteboard, then the chord applied.
    #[test]
    fn paste_picture() {
        use slopty_proto::terminal::PasteChord;
        snap(
            "client_term_paste_picture_command",
            &ClientMsg::Term {
                session: session(),
                req: TermRequest::PastePicture(PasteChord::Command),
            },
        );
        snap(
            "client_term_paste_picture_control",
            &ClientMsg::Term {
                session: session(),
                req: TermRequest::PastePicture(PasteChord::Control(KeyEvent {
                    seq: 8,
                    action: KeyAction::Press,
                    code: KeyCode::V,
                    mods: Mods::CTRL,
                    consumed_mods: Mods::empty(),
                    text: None,
                    unshifted: Some('v'),
                    composing: false,
                    option_as_alt: false,
                })),
            },
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
                bgra: vec![255, 0, 0, 255, 0, 0, 255, 128],
            },
        );
    }

    #[test]
    fn server_links() {
        use slopty_core::WorkerId;
        use slopty_proto::orchestration::{Line, Outcome, TermRef, Verb};
        use slopty_proto::screen::{DisplayInfo, VideoCodec};
        use slopty_proto::server::{
            FromServer, Liveness, Os, Registration, Role, ToServer, WorkerCaps, WorkerInfo,
        };

        let worker = WorkerId::from_uuid(Uuid::from_u128(0x77));
        let caps = WorkerCaps {
            os: Os::MacOs,
            os_version: "26.5".to_owned(),
            arch: "aarch64".to_owned(),
            cpus: 24,
            memory: 128 << 30,
            encoders: vec![VideoCodec::Hevc, VideoCodec::H264],
            displays: vec![DisplayInfo {
                id: slopty_core::DisplayId(1),
                w: 2560.0,
                h: 1440.0,
                scale: 2.0,
                hz: 120.0,
            }],
            agents: Vec::new(),
            can_capture: true,
            can_inject: true,
            virtual_displays: false,
            version: "0.1.0".to_owned(),
            lan: Vec::new(),
            wake_on_lan: None,
        };
        snap(
            "server_worker_hello",
            &ToServer::Hello {
                role: Role::Worker(Box::new(Registration {
                    worker,
                    name: "mac-studio".to_owned(),
                    listen: std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 45570)),
                    caps: caps.clone(),
                    sessions: Vec::new(),
                })),
            },
        );
        let term = TermRef { worker, session: session() };
        snap(
            "server_refused_not_granted",
            &FromServer::Refused(slopty_proto::server::Refusal::NotGranted),
        );
        snap(
            "server_request",
            &FromServer::Request {
                id: 7,
                key: None,
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
                load: 1.5,
                last_seen_ms: WallMs::from_millis(1_790_000_000_000),
            }),
        );
        snap("server_load", &ToServer::Load(0.75));
        let summary = slopty_proto::terminal::SessionSummary {
            id: session(),
            title: "zsh".to_owned(),
            cwd: None,
            repo: None,
            branch: None,
            changes: None,
            started_ms: WallMs::from_millis(1_790_000_000_000),
            cols: 80,
            rows: 24,
            state: slopty_proto::terminal::SessionState::Running,
            viewers: 0,
            command: Vec::new(),
            agent: None,
        };
        snap("server_session_changed", &ToServer::SessionChanged(summary.clone()));
        snap("server_terminals", &FromServer::Terminals(vec![(worker, summary)]));
        snap("server_worker_load", &FromServer::Load { worker, load: 0.75 });
    }

    /// `OSC 9;4` as the tile header and the Dock read it, and a session reopened after its
    /// shell was lost.
    #[test]
    fn progress_and_restored() {
        use slopty_proto::terminal::{Progress, ProgressState, Restored};
        snap(
            "worker_term_progress",
            &TermEvent::Progress(Progress { state: ProgressState::Set, percent: Some(42) }),
        );
        snap(
            "worker_term_progress_indeterminate",
            &TermEvent::Progress(Progress { state: ProgressState::Indeterminate, percent: None }),
        );
        snap(
            "worker_term_restored",
            &TermEvent::Restored(Restored {
                saved_ms: WallMs::from_millis(1_790_000_000_000),
                command: vec!["claude".to_owned()],
            }),
        );
    }

    /// `OSC 22`: the pointer a program asks for over the grid.
    #[test]
    fn pointer_shape() {
        use slopty_proto::terminal::PointerShape;
        snap("worker_term_pointer", &TermEvent::Pointer(PointerShape::Pointer));
        snap("worker_term_pointer_zoom_out", &TermEvent::Pointer(PointerShape::ZoomOut));
    }
}

#[cfg(test)]
mod orchestration {
    use slopty_core::{DisplayId, ItemId, SessionId, WallMs, WindowId, WorkerId};
    use slopty_proto::agent::{AgentEvent, AgentKind, AgentSource, AgentStatus, BlockReason};
    use slopty_proto::codec;
    use slopty_proto::items::{Item, ItemKind};
    use slopty_proto::orchestration::{
        DirEntry, ErrorCode, EventFilter, FileKind, FileStat, Happening, HubEvent, IdempotencyKey,
        Input, ItemRef, Outcome, Size, TermRef, Verb,
    };
    use slopty_proto::screen::{DisplayInfo, WindowInfo};
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
        FromServer::Request { id: 8, key: None, verb }
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
            modified_ms: WallMs::from_millis(1_790_000_000_000),
        };
        snap("server_reply_dir", &reply(Outcome::Dir { entries: vec![entry], total: 2 }));
        let stat = FileStat {
            kind: FileKind::File,
            size: 12,
            modified_ms: WallMs::from_millis(1_790_000_000_000),
            mode: 0o644,
        };
        snap("server_reply_stat", &reply(Outcome::Stat(Some(stat))));
    }

    #[test]
    fn events_and_forgetting() {
        let asked = ToServer::Request {
            id: 9,
            key: None,
            verb: Verb::Events {
                since: Some(41),
                timeout_ms: 60_000,
                filter: EventFilter::AgentNeedsInput,
            },
        };
        snap("server_request_events", &asked);
        let event = HubEvent {
            seq: 41,
            at_ms: WallMs::from_millis(1_790_000_000_000),
            what: Happening::Agent {
                worker: term().worker,
                event: AgentEvent {
                    session: term().session,
                    kind: AgentKind::ClaudeCode,
                    status: AgentStatus::Blocked(BlockReason::Question),
                    agent_session: None,
                    detail: Some("Which branch?".to_owned()),
                    attention: true,
                    source: AgentSource::Hook,
                    since_ms: WallMs::from_millis(1_789_999_990_000),
                },
            },
        };
        // Pushed to a client link as it happens, the same event the log answers with.
        snap("server_event_pushed", &FromServer::Event(event.clone()));
        let answer = Outcome::Events { events: vec![event], next: 42, missed: 0 };
        snap("server_reply_events", &FromServer::Reply { id: 9, outcome: answer });
        let key = Some(IdempotencyKey::new("0199a1b1-c3d4-7000-8000-00000000cafe").expect("a key"));
        let forget =
            ToServer::Request { id: 10, key, verb: Verb::ForgetWorker { worker: term().worker } };
        snap("server_request_forget", &forget);
        let exited = HubEvent {
            seq: 43,
            at_ms: WallMs::from_millis(1_790_000_000_000),
            what: Happening::SessionExited { term: term(), status: -9 },
        };
        let answer = Outcome::Events { events: vec![exited], next: 44, missed: 0 };
        snap("server_reply_session_exited", &FromServer::Reply { id: 11, outcome: answer });
    }

    #[test]
    fn idempotency_keys() {
        let key = IdempotencyKey::new("retry-me").expect("a key");
        let send = Verb::SendInput { term: term(), input: Input::Text("make\n".to_owned()) };
        snap("server_request_keyed", &FromServer::Request { id: 12, key: Some(key), verb: send });
        let lost = Outcome::Error { code: ErrorCode::Interrupted, message: String::new() };
        snap("server_reply_interrupted", &reply(lost));
        let bad: Result<IdempotencyKey, _> =
            codec::decode_body(&codec::encode_body(&"").expect("encodes"));
        assert!(bad.is_err(), "an empty key does not decode");
        assert!(
            IdempotencyKey::new("a b").is_err() && IdempotencyKey::new("x".repeat(129)).is_err()
        );
    }

    #[test]
    fn workspace_items() {
        let worker = term().worker;
        let item = ItemRef { worker, item: ItemId::from_uuid(Uuid::from_u128(0x1_7e5)) };
        let kind = ItemKind::Browser { url: "http://localhost:5173/".to_owned() };
        let open = Verb::OpenItem { worker, kind: kind.clone(), name: Some("app".to_owned()) };
        snap("server_request_open_item", &request(open));
        snap("server_reply_item", &reply(Outcome::Item(item)));
        let listed = Item { id: item.item, kind, sleeping: false, name: None };
        snap("server_reply_items", &reply(Outcome::Items(vec![listed])));
        let rename = Verb::RenameItem { item, name: Some("docs".to_owned()) };
        snap("server_request_rename_item", &request(rename));
        snap("server_request_remove_item", &request(Verb::RemoveItem { item }));
        snap("server_request_point_at", &request(Verb::PointAt { item }));
        snap("server_request_list_windows", &request(Verb::ListWindows { worker }));
        let window = WindowInfo {
            id: WindowId(4242),
            app: "Safari".to_owned(),
            bundle_id: Some("com.apple.Safari".to_owned()),
            title: "Docs".to_owned(),
            x: 0.0,
            y: 25.0,
            w: 1280.0,
            h: 800.0,
            display: DisplayId(1),
            on_screen: true,
        };
        let display = DisplayInfo { id: DisplayId(1), w: 2560.0, h: 1440.0, scale: 2.0, hz: 120.0 };
        let screens = Outcome::Screens { windows: vec![window], displays: vec![display] };
        snap("server_reply_screens", &reply(screens));
    }

    /// The verbs an orchestrating agent reaches another agent and the screen with: a page of
    /// the conversation and the prompt held in it, the answer, a still picture or the refusal of
    /// one, and a file sent up in parts.
    #[test]
    fn agent_verbs() {
        use slopty_core::XferId;
        use slopty_proto::conversation::{
            Body, Clipped, Entry, Meters, Origin, PermissionPrompt, Prompt, Task, ThreadId,
            ToolDetail, Verdict,
        };
        use slopty_proto::orchestration::{ConversationPage, ThreadInfo, UploadPart};
        use slopty_proto::screen::CaptureTarget;

        let worker = term().worker;
        let read = Verb::ReadConversation {
            term: term(),
            thread: ThreadId::Main,
            since: Some(40),
            max: 50,
        };
        snap("server_request_read_conversation", &request(read));
        let text = |s: &str| Clipped { text: s.to_owned(), lines: 1, chars: 7, full: None };
        let prompt =
            Body::Prompt(Prompt { text: text("fix it"), images: Vec::new(), command: None });
        let page = ConversationPage {
            threads: vec![
                ThreadInfo { id: ThreadId::Main, origin: None, entries: 41 },
                ThreadInfo {
                    id: ThreadId::Agent("a1".to_owned()),
                    origin: Some(Origin {
                        tool_use_id: Some("t1".to_owned()),
                        agent_type: Some("Explore".to_owned()),
                        description: None,
                    }),
                    entries: 3,
                },
            ],
            thread: ThreadId::Main,
            entries: vec![Entry {
                id: "u1".to_owned(),
                at_ms: WallMs::from_millis(7),
                body: prompt,
            }],
            start: 40,
            next: 41,
            total: 41,
            tasks: vec![Task {
                id: "1".to_owned(),
                subject: "Build".to_owned(),
                status: "in_progress".to_owned(),
            }],
            meters: Some(Meters { model: Some("Opus".to_owned()), ..Meters::default() }),
            held: vec![PermissionPrompt {
                session: term().session,
                ask: 3,
                tool: "Bash".to_owned(),
                detail: ToolDetail::Other { input: text("{}") },
                suggestions: Vec::new(),
                mode: Some("default".to_owned()),
                asked_ms: WallMs::from_millis(1_790_000_000_000),
                until_ms: WallMs::from_millis(1_790_000_595_000),
            }],
        };
        snap("server_reply_conversation", &reply(Outcome::Conversation(Box::new(page))));
        let key = Some(IdempotencyKey::new("answer-3").expect("a key"));
        let answer = Verb::AnswerPermission {
            term: term(),
            ask: 3,
            verdict: Verdict::Deny { message: "not on main".to_owned(), interrupt: false },
        };
        snap("server_request_answer_permission", &FromServer::Request { id: 8, key, verb: answer });

        let still = Verb::CaptureStill { worker, target: CaptureTarget::Window(WindowId(4242)) };
        snap("server_request_capture_still", &request(still));
        let png = Outcome::Still { png: b"\x89PNG".to_vec(), width: 2560, height: 1600 };
        snap("server_reply_still", &reply(png));
        let refused = Outcome::Error {
            code: ErrorCode::Unsupported,
            message: "no Screen Recording".to_owned(),
        };
        snap("server_reply_unsupported", &reply(refused));

        let upload = XferId::from_uuid(Uuid::from_u128(0x0bad_cafe));
        let path = "~/build/app.tar".to_owned();
        let part = UploadPart::Bytes { offset: 1 << 20, bytes: vec![1, 2, 3] };
        let part = Verb::Upload { worker, path: path.clone(), upload, part };
        snap("server_request_upload_part", &request(part));
        let finish = UploadPart::Finish { size: 3 << 20, digest: [0xab; 32], mode: Some(0o755) };
        let key = Some(IdempotencyKey::new("push-app").expect("a key"));
        let finish = Verb::Upload { worker, path, upload, part: finish };
        assert!(finish.changes(), "the finish is the step a key guards");
        snap("server_request_upload_finish", &FromServer::Request { id: 8, key, verb: finish });
    }

    /// A worker's LAN ports in its caps, a client's wake, the server's ask of a worker on the
    /// sleeping one's subnet, and the answer.
    #[test]
    fn waking_a_sleeping_worker() {
        use std::net::Ipv4Addr;

        use slopty_proto::lan::{LanPort, MacAddr};
        use slopty_proto::server::{Os, WorkerCaps};

        let worker = term().worker;
        let en0 = LanPort {
            interface: "en0".to_owned(),
            mac: MacAddr([0x9c, 0x76, 0x0e, 0x37, 0x42, 0x4e]),
            addr: Ipv4Addr::new(192, 168, 1, 20),
            prefix: 24,
        };
        let caps = WorkerCaps {
            lan: vec![en0.clone()],
            wake_on_lan: Some(true),
            ..WorkerCaps::bare(Os::MacOs)
        };
        snap("server_caps_lan", &ToServer::Caps(caps));
        let wake = Verb::Wake { worker };
        assert!(!wake.changes(), "a second packet wakes nothing the first did not");
        snap("server_client_wake", &ToServer::Request { id: 8, key: None, verb: wake });
        let sender = WorkerId::from_uuid(Uuid::from_u128(0x78));
        let peer = Verb::WakePeer { worker: sender, peer: vec![en0] };
        snap("server_request_wake_peer", &request(peer));
        let sent = Outcome::WakeSent { by: "mac-mini".to_owned(), to: vec!["en0".to_owned()] };
        snap("server_reply_wake_sent", &FromServer::Reply { id: 8, outcome: sent });
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
                (0..rows).map(|row| RowUpdate { row, line: Line::blank(cols).into() }).collect();
            let text: Vec<RowUpdate> = (0..rows)
                .map(|row| RowUpdate {
                    row,
                    line: Line::from_text(&"x".repeat(usize::from(cols)), cols, Style::DEFAULT)
                        .into(),
                })
                .collect();
            // Every row a prompt with a status: the worst case for the per-row mark.
            let prompts: Vec<RowUpdate> = (0..rows)
                .map(|row| {
                    let mut line = Line::blank(cols);
                    line.mark = slopty_grid::SemanticMark::Prompt { exit: Some(1), input: Some(2) };
                    RowUpdate { row, line: line.into() }
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

/// The conversation face: follow and answer, the prompts, and every entry a conversation
/// stream carries.
#[cfg(test)]
mod conversation {
    use slopty_core::{ClientId, SessionId, WallMs};
    use slopty_proto::conversation::{
        AgentDetail, AgentRun, Answer, BashDetail, Blob, Body, Change, Choice, Clipped,
        CommandSource, Compact, ConversationEvent, ConversationRequest, EditDetail, Entry,
        GlobDetail, Grant, GrepDetail, Hunk, Image, Link, Live, LiveId, LiveKind, McpDetail,
        Meters, Note, NoteKind, Output, Part, Patch, PermissionEvent, PermissionPrompt, Prompt,
        Question, QuestionDetail, RateWindow, ReadDetail, ResultStatus, Retry, Settled,
        ShellStatus, SlashCommand, Suggestion, Task, TaskCreateDetail, TaskUpdateDetail, TextRef,
        ThreadId, ToolCall, ToolDetail, ToolResult, Turn, Usage, Verdict, WebFetchDetail,
        WebSearchDetail, WriteDetail, WriteKind,
    };
    use slopty_proto::transfer::UniHead;
    use slopty_proto::{ClientMsg, WorkerMsg, codec};
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

    fn session() -> SessionId {
        SessionId::from_uuid(Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10))
    }

    fn text(s: &str) -> Clipped {
        Clipped { text: s.to_owned(), lines: 1, chars: 3, full: None }
    }

    fn clipped(s: &str, part: Part) -> Clipped {
        Clipped { text: s.to_owned(), lines: 900, chars: 40_000, full: Some(reference(part)) }
    }

    fn reference(part: Part) -> TextRef {
        TextRef { record: "r1".to_owned(), part }
    }

    fn patch() -> Patch {
        Patch {
            hunks: vec![Hunk {
                old_start: 3,
                old_lines: 1,
                new_start: 3,
                new_lines: 1,
                lines: vec!["-a".to_owned(), "+b".to_owned()],
            }],
            added: 1,
            removed: 1,
            clipped_lines: 2,
            full: Some(reference(Part::Patch)),
        }
    }

    fn entry(id: &str, body: Body) -> Change {
        Change::Upsert {
            thread: ThreadId::Main,
            entry: Entry { id: id.to_owned(), at_ms: WallMs::from_millis(7), body },
        }
    }

    fn image(part: Part) -> Image {
        Image {
            digest: "af1349b9".to_owned(),
            media_type: "image/png".to_owned(),
            bytes: 2_048,
            width: 640,
            height: 480,
            at: reference(part),
        }
    }

    fn tool(id: &str, name: &str, detail: ToolDetail, status: ResultStatus) -> Change {
        let result = ToolResult {
            status,
            text: Some(text("out")),
            at_ms: WallMs::from_millis(9),
            images: Vec::new(),
        };
        entry(
            id,
            Body::Tool(Box::new(ToolCall { name: name.to_owned(), detail, result: Some(result) })),
        )
    }

    /// `change`, a tool's upsert, with an image in its result.
    fn pictured(mut change: Change) -> Change {
        if let Change::Upsert { entry, .. } = &mut change
            && let Body::Tool(call) = &mut entry.body
            && let Some(result) = &mut call.result
        {
            result
                .images
                .push(image(Part::Image { tool_use_id: Some(entry.id.clone()), index: 0 }));
        }
        change
    }

    #[test]
    fn requests() {
        let session = session();
        snap("client_follow", &ClientMsg::Conversation(ConversationRequest::Follow { session }));
        snap(
            "client_unfollow",
            &ClientMsg::Conversation(ConversationRequest::Unfollow { session }),
        );
        for (name, verdict) in [
            ("client_answer_allow", Verdict::Allow),
            ("client_answer_always", Verdict::AllowAlways),
            ("client_answer_deny", Verdict::Deny { message: "no".to_owned(), interrupt: true }),
            (
                "client_answer_question",
                Verdict::Answer {
                    answers: vec![Answer {
                        question: "Which layout?".to_owned(),
                        answer: "Split, Stacked".to_owned(),
                    }],
                },
            ),
        ] {
            snap(
                name,
                &ClientMsg::Conversation(ConversationRequest::Answer { session, ask: 3, verdict }),
            );
        }
        snap(
            "client_expand",
            &ClientMsg::Conversation(ConversationRequest::Expand {
                session,
                thread: ThreadId::Agent("a1".to_owned()),
                reference: reference(Part::Input {
                    tool_use_id: "t1".to_owned(),
                    field: "command".to_owned(),
                }),
            }),
        );
        snap(
            "client_expand_image",
            &ClientMsg::Conversation(ConversationRequest::Expand {
                session,
                thread: ThreadId::Main,
                reference: reference(Part::Image { tool_use_id: None, index: 2 }),
            }),
        );
        snap(
            "client_mention_search",
            &ClientMsg::Conversation(ConversationRequest::Search {
                session,
                query: "src/ma".to_owned(),
                limit: 50,
            }),
        );
    }

    #[test]
    fn permissions() {
        let session = session();
        let prompt = PermissionPrompt {
            session,
            ask: 3,
            tool: "Edit".to_owned(),
            detail: ToolDetail::Edit(EditDetail {
                path: "/w/a.rs".to_owned(),
                edits: 1,
                replace_all: false,
                patch: patch(),
            }),
            suggestions: vec![
                Suggestion {
                    grant: Grant::Rules {
                        behavior: "allow".to_owned(),
                        rules: vec!["Bash(ls:*)".to_owned()],
                    },
                    destination: Some("localSettings".to_owned()),
                },
                Suggestion {
                    grant: Grant::Mode { mode: "acceptEdits".to_owned() },
                    destination: None,
                },
                Suggestion {
                    grant: Grant::Directories { directories: vec!["/w".to_owned()] },
                    destination: Some("session".to_owned()),
                },
                Suggestion {
                    grant: Grant::Other { kind: "removeRules".to_owned() },
                    destination: None,
                },
            ],
            mode: Some("default".to_owned()),
            asked_ms: WallMs::from_millis(1_700_000_000_000),
            until_ms: WallMs::from_millis(1_700_000_594_000),
        };
        snap(
            "worker_permission_asked",
            &WorkerMsg::Permission(PermissionEvent::Asked(Box::new(prompt))),
        );
        let by = ClientId::from_uuid(Uuid::from_u128(0x42));
        for (name, outcome) in [
            ("worker_permission_answered", Settled::Answered { verdict: Verdict::AllowAlways, by }),
            ("worker_permission_released", Settled::Released),
            ("worker_permission_withdrawn", Settled::Withdrawn),
        ] {
            snap(
                name,
                &WorkerMsg::Permission(PermissionEvent::Settled { session, ask: 3, outcome }),
            );
        }
    }

    #[test]
    fn stream() {
        snap("uni_conversation", &UniHead::Conversation { session: session() });
        snap("conversation_current", &ConversationEvent::Current);
        snap(
            "conversation_meters",
            &ConversationEvent::Meters(Meters {
                model: Some("Opus".to_owned()),
                model_id: Some("claude-opus-5-5".to_owned()),
                context_used_pct: Some(8.5),
                context_window: Some(200_000),
                cost_usd: Some(0.25),
                five_hour: Some(RateWindow { used_pct: 23.5, resets_at: Some(1_738_425_600) }),
                seven_day: None,
            }),
        );
        let id = |block| LiveId { turn: "t1".to_owned(), step: 1, block };
        snap(
            "conversation_live",
            &ConversationEvent::Live(vec![
                Live::Start { thread: ThreadId::Main, id: id(0), kind: LiveKind::Thinking },
                Live::Append { id: id(0), text: "hmm".to_owned() },
                Live::Start {
                    thread: ThreadId::Agent("a1".to_owned()),
                    id: id(1),
                    kind: LiveKind::Text,
                },
                Live::Start {
                    thread: ThreadId::Main,
                    id: id(2),
                    kind: LiveKind::Tool { id: "toolu_1".to_owned(), name: "Bash".to_owned() },
                },
                Live::Append { id: id(2), text: "{\"command\"".to_owned() },
                Live::Clear { id: id(0) },
            ]),
        );
        snap(
            "conversation_output",
            &ConversationEvent::Output(vec![Output {
                thread: ThreadId::Main,
                call: "t6".to_owned(),
                tail: clipped("ready in 2 s", Part::Output { tool_use_id: "t6".to_owned() }),
                bytes: 90_000,
            }]),
        );
        let picture = reference(Part::Image { tool_use_id: Some("t3".to_owned()), index: 0 });
        snap(
            "conversation_image",
            &ConversationEvent::Image {
                thread: ThreadId::Main,
                reference: picture.clone(),
                blob: Some(Blob { digest: "af1349b9".to_owned(), data: vec![0x89, b'P', b'N'] }),
            },
        );
        snap(
            "conversation_image_gone",
            &ConversationEvent::Image { thread: ThreadId::Main, reference: picture, blob: None },
        );
        snap(
            "conversation_expanded",
            &ConversationEvent::Expanded {
                thread: ThreadId::Main,
                reference: reference(Part::Stdout),
                text: Some(text("all of it")),
            },
        );
        let command = |name: &str, hint: Option<&str>, source| SlashCommand {
            name: name.to_owned(),
            description: format!("{name} it"),
            argument_hint: hint.map(str::to_owned),
            source,
        };
        snap(
            "conversation_commands",
            &ConversationEvent::Commands(vec![
                command("compact", Some("<instructions>"), CommandSource::BuiltIn),
                command("review", None, CommandSource::Personal),
                command("frontend:component", Some("[name]"), CommandSource::Project),
                command("cloudflare:build-agent", None, CommandSource::Plugin),
            ]),
        );
        snap(
            "conversation_found",
            &ConversationEvent::Found {
                query: "ma".to_owned(),
                paths: vec!["src/main.rs".to_owned(), "src/manual/".to_owned()],
            },
        );
    }

    /// One change of each kind, and an entry of every body and every tool.
    #[test]
    fn changes() {
        let ok = ResultStatus::Ok;
        let task = Task {
            id: "1".to_owned(),
            subject: "Write it".to_owned(),
            status: "pending".to_owned(),
        };
        snap(
            "conversation_changes",
            &ConversationEvent::Changes(vec![
                Change::Reset { thread: None },
                entry(
                    "u1",
                    Body::Prompt(Prompt {
                        text: clipped("do it", Part::Block { index: 0 }),
                        images: vec![image(Part::Image { tool_use_id: None, index: 1 })],
                        command: Some("/compact".to_owned()),
                    }),
                ),
                entry("u2:0", Body::Text(text("yes"))),
                entry("u2:1", Body::Thinking(text("hmm"))),
                entry(
                    "u3",
                    Body::Compact(Compact {
                        trigger: Some("auto".to_owned()),
                        pre_tokens: Some(150_000),
                        post_tokens: Some(9_000),
                        summary: Some(text("so far")),
                    }),
                ),
                entry("u4", Body::Interrupted { during_tool: true }),
                entry(
                    "u5",
                    Body::Note(Note {
                        kind: NoteKind::ApiError,
                        text: text("529"),
                        retry: Some(Retry { attempt: 2, max: 10, in_ms: 1_100 }),
                    }),
                ),
                entry(
                    "u6",
                    Body::Note(Note { kind: NoteKind::Command, text: text("ok"), retry: None }),
                ),
                entry(
                    "u7",
                    Body::Note(Note { kind: NoteKind::Info, text: text("fyi"), retry: None }),
                ),
                entry(
                    "u8",
                    Body::Note(Note { kind: NoteKind::Hook, text: text("no"), retry: None }),
                ),
                entry("u9", Body::Rewound { dropped: 6 }),
                Change::Remove { thread: ThreadId::Agent("a1".to_owned()), id: "u0".to_owned() },
                Change::Tasks { thread: ThreadId::Main, tasks: vec![task.clone()] },
                Change::Reset { thread: Some(ThreadId::Agent("a1".to_owned())) },
                Change::Turn {
                    thread: ThreadId::Main,
                    turn: Turn {
                        prompt: "u1".to_owned(),
                        started_ms: WallMs::from_millis(7),
                        ended_ms: Some(WallMs::from_millis(9)),
                        models: vec!["claude-opus-5-5".to_owned()],
                        requests: 3,
                        usage: Usage {
                            input: 12,
                            cache_read: 40_000,
                            cache_write: 800,
                            output: 900,
                            thinking: 300,
                        },
                        context_tokens: Some(41_200),
                        mode: Some("plan".to_owned()),
                        stop: Some("end_turn".to_owned()),
                    },
                },
            ]),
        );
        let bash = BashDetail {
            command: clipped(
                "cargo test",
                Part::Input { tool_use_id: "t6".to_owned(), field: "command".to_owned() },
            ),
            description: Some("Run tests".to_owned()),
            background: true,
            task_id: Some("b1".to_owned()),
            status: ShellStatus::Failed,
            exit_code: Some(101),
            stdout: Some(clipped("…ok", Part::Stdout)),
            stderr: Some(clipped("…err", Part::Stderr)),
            output_file: Some("/tmp/b1.out".to_owned()),
            finished_ms: Some(WallMs::from_millis(12)),
        };
        snap(
            "conversation_tools",
            &ConversationEvent::Changes(vec![
                tool(
                    "t1",
                    "Edit",
                    ToolDetail::Edit(EditDetail {
                        path: "/w/a.rs".to_owned(),
                        edits: 2,
                        replace_all: true,
                        patch: patch(),
                    }),
                    ok,
                ),
                tool(
                    "t2",
                    "Write",
                    ToolDetail::Write(WriteDetail {
                        path: "/w/b.rs".to_owned(),
                        lines: 12,
                        kind: WriteKind::Overwrite,
                        patch: patch(),
                    }),
                    ok,
                ),
                pictured(tool(
                    "t3",
                    "Read",
                    ToolDetail::Read(ReadDetail {
                        path: "/w/a.rs".to_owned(),
                        offset: Some(10),
                        limit: Some(20),
                        start_line: Some(10),
                        lines: Some(20),
                        total_lines: Some(300),
                    }),
                    ok,
                )),
                tool(
                    "t4",
                    "Grep",
                    ToolDetail::Grep(GrepDetail {
                        pattern: "fn".to_owned(),
                        path: Some("/w".to_owned()),
                        glob: Some("*.rs".to_owned()),
                        mode: Some("content".to_owned()),
                        files: Some(3),
                        lines: Some(9),
                    }),
                    ok,
                ),
                tool(
                    "t5",
                    "Glob",
                    ToolDetail::Glob(GlobDetail {
                        pattern: "**/*.rs".to_owned(),
                        path: None,
                        files: Some(40),
                        truncated: true,
                    }),
                    ok,
                ),
                tool("t6", "Bash", ToolDetail::Bash(bash), ResultStatus::Error),
                tool(
                    "t7",
                    "WebFetch",
                    ToolDetail::WebFetch(WebFetchDetail {
                        url: "https://x.dev".to_owned(),
                        prompt: Some("sum".to_owned()),
                        code: Some(200),
                        bytes: Some(512),
                    }),
                    ok,
                ),
                tool(
                    "t8",
                    "WebSearch",
                    ToolDetail::WebSearch(WebSearchDetail {
                        query: "rust".to_owned(),
                        results: Some(10),
                        links: vec![Link {
                            title: "Rust".to_owned(),
                            url: "https://rust-lang.org".to_owned(),
                        }],
                    }),
                    ok,
                ),
                tool(
                    "t9",
                    "Agent",
                    ToolDetail::Agent(AgentDetail {
                        agent_id: Some("a1".to_owned()),
                        agent_type: Some("Explore".to_owned()),
                        description: Some("Look".to_owned()),
                        prompt: text("find it"),
                        background: false,
                        status: AgentRun::Completed,
                        report: Some(text("found")),
                        tokens: Some(1_200),
                        tool_uses: Some(4),
                        duration_ms: Some(3_000),
                    }),
                    ok,
                ),
                tool(
                    "t10",
                    "TaskCreate",
                    ToolDetail::TaskCreate(TaskCreateDetail {
                        task_id: Some("1".to_owned()),
                        subject: "Write it".to_owned(),
                        description: None,
                    }),
                    ok,
                ),
                tool(
                    "t11",
                    "TaskUpdate",
                    ToolDetail::TaskUpdate(TaskUpdateDetail {
                        task_id: "1".to_owned(),
                        from: Some("pending".to_owned()),
                        to: Some("completed".to_owned()),
                        subject: None,
                        fields: vec!["status".to_owned()],
                    }),
                    ok,
                ),
                tool("t12", "TodoWrite", ToolDetail::TodoWrite { todos: vec![task] }, ok),
                tool(
                    "t13",
                    "AskUserQuestion",
                    ToolDetail::Question(QuestionDetail {
                        questions: vec![Question {
                            text: "Which?".to_owned(),
                            header: Some("Pick".to_owned()),
                            options: vec![
                                Choice { label: "a".to_owned(), description: None },
                                Choice {
                                    label: "b".to_owned(),
                                    description: Some("the other".to_owned()),
                                },
                            ],
                            multi_select: false,
                        }],
                        answers: vec![Answer {
                            question: "Which?".to_owned(),
                            answer: "a".to_owned(),
                        }],
                    }),
                    ok,
                ),
                tool(
                    "t14",
                    "ExitPlanMode",
                    ToolDetail::Plan { plan: text("1. do") },
                    ResultStatus::Rejected,
                ),
                tool(
                    "t15",
                    "mcp__gh__issue",
                    ToolDetail::Mcp(McpDetail {
                        server: "gh".to_owned(),
                        tool: "issue".to_owned(),
                        input: text("{}"),
                    }),
                    ok,
                ),
                tool(
                    "t16",
                    "Skill",
                    ToolDetail::Other {
                        input: clipped("{\"x\":1}", Part::Result { tool_use_id: "t16".to_owned() }),
                    },
                    ok,
                ),
            ]),
        );
    }
}

/// The worker's local control socket: one JSON line per request and reply, as the CLI, the hook
/// relay and the app write and read them.
#[cfg(test)]
mod ctl {
    use std::net::IpAddr;

    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use slopty_core::{DisplayId, SessionId, WallMs, WindowId, WorkerId};
    use slopty_proto::ctl::{
        CtlReply, CtlRequest, Decision, Health, LtrStats, PasteboardAccess, PermissionAnswer,
        PermissionAsk, Quantiles, ScreenStats, ScreenSummary, Tailscale,
    };
    use slopty_proto::screen::{CaptureTarget, VideoCodec};
    use slopty_proto::server::{Os, WorkerCaps};
    use slopty_proto::tailnet::BackendState;
    use slopty_proto::terminal::{SessionState, SessionSummary};
    use uuid::Uuid;

    fn session() -> SessionId {
        SessionId::from_uuid(Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10))
    }

    /// Pins the line and reads it back to the same value.
    #[track_caller]
    fn snap<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(name: &str, msg: &T) {
        let line = serde_json::to_string(msg).expect("encodes");
        assert_eq!(serde_json::from_str::<T>(&line).ok().as_ref(), Some(msg), "{name} reads back");
        insta::assert_snapshot!(name, line);
    }

    fn health() -> Health {
        Health {
            version: "0.1.0".to_owned(),
            exe: "/Applications/Slopty.app/Contents/MacOS/slopty-worker".to_owned(),
            caps: WorkerCaps {
                os: Os::MacOs,
                os_version: "26.5".to_owned(),
                arch: "aarch64".to_owned(),
                cpus: 12,
                memory: 32 << 30,
                encoders: vec![VideoCodec::Hevc],
                displays: Vec::new(),
                agents: Vec::new(),
                can_capture: true,
                can_inject: false,
                virtual_displays: false,
                version: "0.1.0".to_owned(),
                lan: Vec::new(),
                wake_on_lan: None,
            },
            listen: "[::]:45550".to_owned(),
            allow: vec!["10.0.0.0/8".to_owned()],
            tailscale: Tailscale::Up {
                node: "mac-studio.tail1234.ts.net".to_owned(),
                ip: Some(IpAddr::from([100, 64, 0, 7])),
            },
            pasteboard: PasteboardAccess::Allowed,
            clients: 2,
            sessions: 5,
            uptime_secs: 86_400,
        }
    }

    const fn quantiles(p50_us: u64, p95_us: u64, max_us: u64) -> Quantiles {
        Quantiles { n: 600, p50_us, p95_us, max_us }
    }

    #[test]
    fn requests() {
        snap("ctl_request_status", &CtlRequest::Status);
        snap("ctl_request_doctor", &CtlRequest::Doctor);
        snap("ctl_request_screens", &CtlRequest::Screens);
        snap(
            "ctl_request_hook",
            &CtlRequest::Hook {
                session: session(),
                payload: r#"{"hook_event_name":"Stop"}"#.to_owned(),
            },
        );
        snap(
            "ctl_request_permission",
            &CtlRequest::Permission(PermissionAsk {
                session: session(),
                payload: r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#.to_owned(),
                wait_ms: 595_000,
            }),
        );
    }

    #[test]
    fn status_and_doctor() {
        snap(
            "ctl_reply_status",
            &CtlReply::Status {
                id: WorkerId::from_uuid(Uuid::from_u128(0x77)),
                name: "mac-studio".to_owned(),
                sessions: vec![SessionSummary {
                    id: session(),
                    title: "zsh".to_owned(),
                    cwd: Some("/w/slopty".to_owned()),
                    repo: None,
                    branch: None,
                    changes: None,
                    started_ms: WallMs::from_millis(1_790_000_000_000),
                    cols: 80,
                    rows: 24,
                    state: SessionState::Exited { status: 1 },
                    viewers: 0,
                    command: vec!["/bin/zsh".to_owned()],
                    agent: None,
                }],
            },
        );
        snap("ctl_reply_doctor", &CtlReply::Doctor(Box::new(health())));
        snap("ctl_health_without_tailscale", &Health { tailscale: Tailscale::Absent, ..health() });
        snap("ctl_tailscale_down", &Tailscale::Down { backend: BackendState::NeedsLogin });
        let error = "tailscale's local API did not answer within 2s".to_owned();
        snap("ctl_tailscale_unreachable", &Tailscale::Unreachable { error });
    }

    #[test]
    fn screens() {
        let stats = ScreenStats {
            captured: 1_200,
            dropped: 3,
            withheld: 4,
            suspected: 5,
            suspicions: 6,
            siblings: 7,
            encoded: 1_190,
            datagrams: 9_000,
            queue_full: 1,
            heartbeats: 40,
            refreshes: 2,
            keyframes_deferred: 1,
            latency_max_us: 9_000,
            latency_sum_us: 4_000_000,
            audio_packets: 500,
            bitrate_bps: 20_000_000,
            capture: quantiles(1_000, 2_000, 5_000),
            encode: quantiles(2_000, 4_000, 10_000),
            beat_gap: quantiles(250_000, 500_000, 1_250_000),
            bounds: quantiles(300, 600, 1_500),
            beat_gap_worst_us: 1_300_000,
            cropped: 10,
            ltr: LtrStats {
                offered: 30,
                acked: 28,
                refreshes_idr: 1,
                refreshes_delta: 2,
                usable: true,
                usable_age_us: 150_000,
            },
            encoder_bps: 18_000_000,
            repaired: 11,
            laned: 12,
            on_crop: true,
        };
        snap(
            "ctl_reply_screens",
            &CtlReply::Screens {
                live: vec![ScreenSummary {
                    client: "iPad".to_owned(),
                    stream: 3,
                    target: CaptureTarget::Window(WindowId(4_242)),
                    stats,
                }],
                closed: vec![ScreenSummary {
                    client: "MacBook".to_owned(),
                    stream: 1,
                    target: CaptureTarget::Display(DisplayId(1)),
                    stats: ScreenStats::default(),
                }],
            },
        );
    }

    /// Every decision a permission request can get.
    #[test]
    fn permissions() {
        let reply = |decision| CtlReply::Permission(PermissionAnswer { decision });
        snap("ctl_reply_permission_pass", &reply(Decision::Pass));
        snap("ctl_reply_permission_allow", &reply(Decision::Allow { updated_input: None }));
        snap(
            "ctl_reply_permission_answer",
            &reply(Decision::Allow {
                updated_input: Some(serde_json::json!({
                    "answers": { "Which layout?": "Split" },
                    "questions": [{
                        "header": "Layout", "multiSelect": false, "question": "Which layout?",
                        "options": [{ "label": "Split" }, { "label": "Stacked" }]
                    }]
                })),
            }),
        );
        snap(
            "ctl_reply_permission_always",
            &reply(Decision::AllowAlways {
                // Keys in sorted order: whether a `Value` keeps insertion order depends on
                // serde_json's `preserve_order`, which feature unification turns on in some
                // builds and not others.
                updated_permissions: vec![serde_json::json!({
                    "behavior": "allow", "destination": "localSettings",
                    "rules": [{ "ruleContent": "cargo test:*", "toolName": "Bash" }],
                    "type": "addRules"
                })],
            }),
        );
        snap(
            "ctl_reply_permission_deny",
            &reply(Decision::Deny { message: "Not now.".to_owned(), interrupt: true }),
        );
    }

    #[test]
    fn outcomes() {
        snap("ctl_reply_ok", &CtlReply::Ok { changed: true });
        snap("ctl_reply_error", &CtlReply::Error { message: "no such session".to_owned() });
    }

    /// What the doctor says of a worker whose clipboard reads wait on the person, and of one
    /// that sleeps through a magic packet.
    #[test]
    fn pasteboard_access_and_wake_for_network_access() {
        let mut health = health();
        health.pasteboard = PasteboardAccess::NotAskedYet;
        health.caps.wake_on_lan = Some(false);
        snap("ctl_health_pasteboard_asks_and_no_wake", &health);
        let every = [
            PasteboardAccess::Allowed,
            PasteboardAccess::NotAskedYet,
            PasteboardAccess::Asks,
            PasteboardAccess::Denied,
        ];
        snap("ctl_pasteboard_access", &every.to_vec());
    }
}
