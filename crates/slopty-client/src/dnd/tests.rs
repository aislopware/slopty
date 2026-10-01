use pretty_assertions::assert_eq;
use slopty_platform::pasteboard::Memory;
use slopty_proto::transfer::{ClipFormat, INLINE_CLIP_BYTES};

use super::*;

fn url(path: &std::path::Path) -> String {
    url::Url::from_file_path(path).unwrap().to_string()
}

/// A drag's pasteboard reads as the worker's drag will carry it: a file it names, by its name
/// with the path kept here for the upload; a file it promises, by its content type; text
/// inline, and a picture past the inline budget listed by its size and sent up beside. A URL
/// to a file that is not here, and an item with nothing that travels, are left off.
#[test]
fn every_item_reads_as_the_worker_will_carry_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes 1.txt");
    std::fs::write(&file, b"twelve bytes").unwrap();
    let gone = dir.path().join("gone.txt");
    let picture = vec![7_u8; INLINE_CLIP_BYTES + 1];
    let (file_url, gone_url) = (url(&file), url(&gone));
    let board = Memory::default();
    board.copy_items(&[
        &[("public.file-url", file_url.as_bytes())],
        &[("public.file-url", gone_url.as_bytes())],
        &[
            ("com.apple.pasteboard.promised-file-content-type", b"public.jpeg"),
            ("com.apple.NSFilePromiseItemMetaData", b""),
        ],
        &[("public.utf8-plain-text", b"fox"), ("public.png", &picture)],
        &[("dyn.ah62d4rv4gu8y", b"x")],
    ]);
    let read = read(&board);
    assert_eq!(read.files, vec![file]);
    assert_eq!(read.items.len(), 3, "{:?}", read.items);
    let meta = read.items[0].file.as_ref().unwrap();
    assert_eq!((meta.name.as_str(), meta.size, meta.folder), ("notes 1.txt", 12, false));
    assert_eq!(meta.path, None, "the client's path means nothing on the worker");
    assert_eq!(read.items[1].promised.as_deref(), Some("public.jpeg"));
    assert!(read.promises());
    let reps = &read.items[2].reps;
    assert_eq!(reps[0].kind, ClipType::Format(ClipFormat::Text));
    assert_eq!(reps[0].inline.as_deref(), Some(&b"fox"[..]));
    assert_eq!(reps[1].kind, ClipType::Format(ClipFormat::Png));
    assert_eq!((reps[1].inline.as_ref(), reps[1].size), (None, Some(picture.len() as u64)));
    assert_eq!(read.pushes.len(), 1);
    assert_eq!((read.pushes[0].item, read.pushes[0].bytes.len()), (2, picture.len()));
}

/// The client's drag shows what the worker last said a drop would do, a copy until it speaks;
/// a move to the point last sent goes no further.
#[test]
fn the_badge_is_the_workers_last_answer() {
    let (mut hover, enter) = Hover::enter((10.0, 20.0), &Read::default(), DragOps::COPY);
    assert!(matches!(enter, DragInput::Enter { x: 10.0, y: 20.0, .. }));
    assert_eq!(hover.op(), DragOp::Copy, "the drop decides until the worker speaks");
    assert_eq!(hover.moved((10.0, 20.0)), None, "where the entry went");
    let drag = hover.drag();
    assert_eq!(hover.moved((11.0, 20.0)), Some(DragInput::Move { drag, x: 11.0, y: 20.0 }));
    hover.heard(&DragEvent::Operation { drag, op: DragOp::None });
    assert_eq!(hover.op(), DragOp::None);
    hover.heard(&DragEvent::Operation { drag: DragId::new(), op: DragOp::Copy });
    assert_eq!(hover.op(), DragOp::None, "another drag's answer");
    hover.heard(&DragEvent::Operation { drag, op: DragOp::Move });
    assert_eq!(hover.op(), DragOp::None, "an operation the source does not allow is none");
    hover.heard(&DragEvent::Operation { drag, op: DragOp::Copy });
    assert_eq!(hover.op(), DragOp::Copy);
}

/// A drop where the worker said nothing would take it is refused here, so the drag slides back
/// as over a local target that refuses, and nothing is sent; leaving then is told once.
#[test]
fn a_refused_drop_slides_back_and_sends_nothing() {
    let (mut hover, _enter) = Hover::enter((0.0, 0.0), &Read::default(), DragOps::COPY);
    let drag = hover.drag();
    hover.heard(&DragEvent::Operation { drag, op: DragOp::None });
    assert_eq!(hover.drop((1.0, 1.0), Vec::new()), None);
    assert_eq!(hover.phase(), Phase::Hovering);
    assert_eq!(hover.leave(), Some(DragInput::Leave { drag }));
    assert_eq!(hover.leave(), None, "once");
    assert_eq!(hover.moved((5.0, 5.0)), None, "a drag that left sends no moves");
}

/// A drop is sent with its point and promised files, its point kept for the ring while the
/// worker lands it, and the worker's end says how it went: landed, refused by the target, or
/// failed with why. Only the first end counts.
#[test]
fn the_end_says_how_the_drop_landed() {
    for (op, error, outcome) in [
        (DragOp::Copy, None, Outcome::Landed(DragOp::Copy)),
        (DragOp::None, None, Outcome::Refused),
        (
            DragOp::None,
            Some("x.bin: the disk is full"),
            Outcome::Failed("x.bin: the disk is full".to_owned()),
        ),
    ] {
        let (mut hover, _enter) = Hover::enter((0.0, 0.0), &Read::default(), DragOps::COPY);
        let drag = hover.drag();
        let promised = vec![Promised { item: 0, file: None }];
        let dropped = hover.drop((3.0, 4.0), promised.clone());
        assert_eq!(dropped, Some(DragInput::Drop { drag, x: 3.0, y: 4.0, promised }));
        assert_eq!(hover.dropped(), Some((3.0, 4.0)));
        assert_eq!(hover.drop((3.0, 4.0), Vec::new()), None, "dropped once");
        let error = error.map(str::to_owned);
        assert_eq!(
            hover.heard(&DragEvent::Ended { drag, op, error: error.clone() }),
            Some(outcome)
        );
        assert_eq!(hover.dropped(), None, "the ring goes");
        assert_eq!(hover.heard(&DragEvent::Ended { drag, op, error }), None);
        assert_eq!(hover.leave(), None, "nothing to leave once it ended");
    }
}
