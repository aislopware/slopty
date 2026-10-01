//! Golden byte snapshots of drag and drop (`slopty_proto::drag`): the client's drag over a
//! stream and the worker's answers, a drag's transfer and representations, and the worker's
//! drag helper socket (`slopty_proto::dnd`, local, so outside the wire fingerprint). A changed
//! snapshot is a wire change: accept it deliberately (`cargo insta review`).

#[cfg(test)]
mod golden_drag {
    use slopty_core::{StreamId, WallMs, XferId};
    use slopty_proto::drag::{
        DragEvent, DragId, DragInput, DragItem, DragOp, DragOps, FileMeta, Promised,
    };
    use slopty_proto::screen::{ScreenEvent, ScreenInput, ScreenRequest};
    use slopty_proto::transfer::{
        BulkHeader, ClipFormat, ClipMsg, ClipType, Dest, Purpose, Rep, RepRef, Source, UniHead,
        XferMsg,
    };
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

    fn drag() -> DragId {
        DragId::from_uuid(Uuid::from_u128(0xd7a6))
    }

    const STREAM: StreamId = StreamId(3);

    fn input(input: DragInput) -> ClientMsg {
        ClientMsg::Screen(ScreenRequest::Input { stream: STREAM, input: ScreenInput::Drag(input) })
    }

    fn event(event: DragEvent) -> WorkerMsg {
        WorkerMsg::Screen(ScreenEvent::Drag { stream: STREAM, event })
    }

    fn file(name: &str, size: u64, path: Option<&str>) -> FileMeta {
        FileMeta {
            name: name.to_owned(),
            size,
            folder: false,
            mode: 0o644,
            mtime_ms: WallMs::from_millis(1_700_000_000_000),
            path: path.map(str::to_owned),
        }
    }

    fn text(bytes: &[u8]) -> Rep {
        Rep {
            kind: ClipType::Format(ClipFormat::Text),
            size: Some(bytes.len() as u64),
            hash: Some([2; 32]),
            inline: Some(bytes.to_vec()),
        }
    }

    #[test]
    fn drop_in() {
        let folder = FileMeta { folder: true, mode: 0o755, ..file("photos", 1 << 20, None) };
        snap(
            "client_screen_drag_enter",
            &input(DragInput::Enter {
                drag: drag(),
                x: 640.5,
                y: 360.0,
                allowed: DragOps::COPY | DragOps::LINK,
                items: vec![
                    DragItem {
                        file: Some(file("notes.txt", 12, None)),
                        promised: None,
                        reps: vec![],
                    },
                    DragItem { file: Some(folder), promised: None, reps: vec![] },
                    DragItem { file: None, promised: Some("public.jpeg".to_owned()), reps: vec![] },
                    DragItem {
                        file: None,
                        promised: None,
                        reps: vec![
                            Rep {
                                kind: ClipType::Format(ClipFormat::Png),
                                size: Some(2 << 20),
                                hash: None,
                                inline: None,
                            },
                            text(b"fox"),
                        ],
                    },
                ],
            }),
        );
        snap(
            "client_screen_drag_move",
            &input(DragInput::Move { drag: drag(), x: 700.0, y: 12.25 }),
        );
        snap("client_screen_drag_leave", &input(DragInput::Leave { drag: drag() }));
        snap(
            "client_screen_drag_drop",
            &input(DragInput::Drop {
                drag: drag(),
                x: 701.0,
                y: 13.0,
                promised: vec![
                    Promised { item: 2, file: Some(file("IMG_0042.jpeg", 3 << 20, None)) },
                    Promised { item: 4, file: None },
                ],
            }),
        );
        snap(
            "worker_screen_drag_operation",
            &event(DragEvent::Operation { drag: drag(), op: DragOp::Copy }),
        );
        snap(
            "worker_screen_drag_ended",
            &event(DragEvent::Ended { drag: drag(), op: DragOp::Copy, error: None }),
        );
        snap(
            "worker_screen_drag_ended_refused",
            &event(DragEvent::Ended {
                drag: drag(),
                op: DragOp::None,
                error: Some("another drag is crossing this worker".to_owned()),
            }),
        );
    }

    #[test]
    fn drag_out() {
        let found = DragItem {
            file: Some(file("report.pdf", 4096, Some("/Users/c/Desktop/report.pdf"))),
            promised: None,
            reps: vec![],
        };
        snap(
            "worker_screen_drag_out_began",
            &event(DragEvent::OutBegan {
                drag: drag(),
                items: vec![
                    found.clone(),
                    DragItem { file: None, promised: Some("public.png".to_owned()), reps: vec![] },
                ],
            }),
        );
        snap("client_screen_drag_catch", &input(DragInput::Catch { drag: drag() }));
        snap(
            "worker_screen_drag_out_caught",
            &event(DragEvent::OutCaught {
                drag: drag(),
                items: vec![
                    found,
                    DragItem { file: None, promised: None, reps: vec![text(b"fox")] },
                ],
            }),
        );
        snap(
            "worker_screen_drag_out_failed",
            &event(DragEvent::OutFailed {
                drag: drag(),
                error: "the drag ended before it was caught".to_owned(),
            }),
        );
    }

    #[test]
    fn transfer() {
        let xfer = XferId::from_uuid(Uuid::from_u128(9));
        snap(
            "client_xfer_begin_drag",
            &ClientMsg::Xfer(XferMsg::Begin {
                xfer,
                dest: Some(Dest::Drag(drag())),
                files: 3,
                bytes: 1 << 20,
            }),
        );
        let rep = RepRef {
            source: Source::Drag(drag()),
            item: 3,
            kind: ClipType::Format(ClipFormat::Png),
        };
        snap(
            "uni_bulk_drag_rep",
            &UniHead::Bulk(BulkHeader {
                xfer,
                purpose: Purpose::Rep { rep: rep.clone() },
                name: String::new(),
                size: 2 << 20,
                mtime_ms: WallMs::ZERO,
                mode: 0o600,
                offset: 0,
            }),
        );
        snap(
            "client_clip_data_drag",
            &ClientMsg::Clip(ClipMsg::Data { rep, bytes: vec![0x89, b'P', b'N', b'G'] }),
        );
    }
}

/// The worker's drag helper socket: local, so its goldens stay outside the wire fingerprint
/// (`wire::fingerprint`), and pinned all the same, since the worker and its helper are one
/// binary run twice and a change here is a change of both.
#[cfg(test)]
mod dnd {
    use slopty_proto::codec;
    use slopty_proto::dnd::{CaughtData, FromHelper, Given, SourceItem, ToHelper};
    use slopty_proto::drag::{DragId, DragOp};
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

    fn drag() -> DragId {
        DragId::from_uuid(Uuid::from_u128(0xd7a6))
    }

    #[test]
    fn to_helper() {
        snap(
            "dnd_source_at",
            &ToHelper::SourceAt {
                drag: drag(),
                x: 812.5,
                y: 410.0,
                items: vec![
                    SourceItem {
                        file: Some("/Users/c/.slopty/drop/d7a6/notes.txt".to_owned()),
                        is_file: true,
                        types: vec![],
                        given: vec![],
                    },
                    SourceItem {
                        file: None,
                        is_file: false,
                        types: vec!["public.png".to_owned(), "public.utf8-plain-text".to_owned()],
                        given: vec![Given {
                            uti: "public.utf8-plain-text".to_owned(),
                            bytes: b"fox".to_vec(),
                        }],
                    },
                ],
            },
        );
        snap(
            "dnd_data",
            &ToHelper::Data {
                drag: drag(),
                item: 1,
                uti: "public.png".to_owned(),
                bytes: Some(vec![0x89, b'P', b'N', b'G']),
            },
        );
        snap("dnd_stop", &ToHelper::Stop { drag: drag() });
        snap(
            "dnd_catcher_at",
            &ToHelper::CatcherAt {
                drag: drag(),
                x: 4.0,
                y: 900.0,
                dir: "/Users/c/.slopty/drag/d7a6".to_owned(),
            },
        );
    }

    #[test]
    fn from_helper() {
        snap("dnd_ready", &FromHelper::Ready { drag: drag() });
        snap("dnd_began", &FromHelper::Began { drag: drag() });
        snap("dnd_operation", &FromHelper::Operation { drag: drag(), op: DragOp::Link });
        snap("dnd_ended", &FromHelper::Ended { drag: drag(), op: DragOp::Copy });
        snap(
            "dnd_asked",
            &FromHelper::Asked { drag: drag(), item: 0, uti: "public.file-url".to_owned() },
        );
        snap(
            "dnd_caught",
            &FromHelper::Caught {
                drag: drag(),
                files: vec!["/Users/c/Desktop/report.pdf".to_owned()],
                data: vec![
                    CaughtData {
                        item: 1,
                        uti: "public.utf8-plain-text".to_owned(),
                        bytes: Some(b"fox".to_vec()),
                        size: 3,
                    },
                    CaughtData {
                        item: 1,
                        uti: "com.adobe.pdf".to_owned(),
                        bytes: None,
                        size: 64 << 20,
                    },
                ],
                promises: 1,
            },
        );
        snap(
            "dnd_promised",
            &FromHelper::Promised {
                drag: drag(),
                path: Some("/Users/c/.slopty/drag/d7a6/IMG_0042.jpeg".to_owned()),
                error: None,
            },
        );
    }
}
