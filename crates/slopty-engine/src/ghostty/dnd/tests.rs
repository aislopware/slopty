use slopty_proto::input::CellMetrics;
use slopty_proto::terminal::TermSize;

use super::*;
use crate::EngineConfig;

fn engine() -> GhosttyEngine {
    GhosttyEngine::new(EngineConfig {
        size: TermSize {
            cols: 40,
            rows: 6,
            metrics: CellMetrics { cell_width: 8, cell_height: 16 },
        },
        scrollback_lines: 100,
    })
    .unwrap()
}

/// What the engine wrote back to the program, and its other events.
fn drained(e: &GhosttyEngine) -> (String, Vec<EngineEvent>) {
    let mut written = String::new();
    let mut others = Vec::new();
    for ev in e.drain_events() {
        match ev {
            EngineEvent::PtyWrite(b) => written.push_str(&String::from_utf8_lossy(&b)),
            other => others.push(other),
        }
    }
    (written, others)
}

const AT: DropPoint = DropPoint { col: 3, row: 2, x: 28, y: 40, copy: true, moves: false };
const URIS: &str = "text/uri-list";

/// A program that never asked for drops is told nothing, and a drag goes on as before.
#[test]
fn a_drag_over_a_program_that_did_not_ask_tells_nothing() {
    let mut e = engine();
    assert!(!e.drop_target().unwrap());
    assert!(!e.drag_over(AT, &[URIS.to_owned()]).unwrap());
    assert!(!e.dropped(AT, Vec::new()).unwrap());
    assert_eq!(drained(&e), (String::new(), Vec::new()));
}

/// The whole drop: the program asks, hears the drag, accepts it, gets the drop, and reads the
/// worker's file list once the upload gives it, then concludes.
#[test]
fn a_drop_waits_for_its_files_and_answers_the_program() {
    let mut e = engine();
    e.write(b"\x1b]72;t=a;text/uri-list\x1b\\");
    assert_eq!(drained(&e).1, [EngineEvent::DropTarget { accepts: true }]);
    assert!(e.drop_target().unwrap());

    let mimes = [URIS.to_owned(), "text/plain".to_owned()];
    assert!(e.drag_over(AT, &mimes).unwrap());
    let (told, _) = drained(&e);
    assert!(told.starts_with("\x1b]72;t=m:x=3:y=2:X=28:Y=40:o=1"), "{told:?}");
    e.write(b"\x1b]72;t=m:o=1;text/uri-list\x1b\\");
    assert_eq!(
        drained(&e).1,
        [EngineEvent::DropAccepted {
            operation: DropOperation::Copy,
            mimes: vec![URIS.to_owned()]
        }]
    );

    let reps = vec![
        DropRep { mime: URIS.to_owned(), data: None },
        DropRep { mime: "text/plain".to_owned(), data: Some(b"a.png".to_vec()) },
    ];
    assert!(e.dropped(AT, reps).unwrap());
    assert!(drained(&e).0.starts_with("\x1b]72;t=M:x=3:y=2"));
    // The text is here and answers at once; the files are still uploading.
    e.write(b"\x1b]72;t=r:x=2\x1b\\");
    // "a.png"
    assert!(drained(&e).0.contains("t=r:x=2:m=0;YS5wbmc="));
    e.write(b"\x1b]72;t=r:x=1\x1b\\");
    assert_eq!(drained(&e).0, "", "waits for the upload");
    e.drop_data(URIS, Some(b"file:///tmp/drop/a.png\r\n".to_vec())).unwrap();
    // "file:///tmp/drop/a.png\r\n"
    assert!(drained(&e).0.contains("t=r:x=1:m=0;ZmlsZTovLy90bXAvZHJvcC9hLnBuZw0K"));

    e.write(b"\x1b]72;t=r:o=1\x1b\\");
    assert_eq!(drained(&e).1, [EngineEvent::DropConcluded { operation: DropOperation::Copy }]);
    e.write(b"\x1b]72;t=A\x1b\\");
    assert_eq!(drained(&e).1, [EngineEvent::DropTarget { accepts: false }]);
}

/// Files that never arrive fail the request rather than leave the program waiting.
#[test]
fn a_failed_upload_fails_the_request() {
    let mut e = engine();
    e.write(b"\x1b]72;t=a\x1b\\");
    let _told = drained(&e);
    assert!(e.dropped(AT, vec![DropRep { mime: URIS.to_owned(), data: None }]).unwrap());
    e.write(b"\x1b]72;t=r:x=1\x1b\\");
    let _told = drained(&e);
    e.drop_data(URIS, None).unwrap();
    let (told, _) = drained(&e);
    assert!(told.contains("t=R:x=1") && told.contains("EIO"), "{told:?}");
}
