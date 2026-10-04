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
    assert_eq!(e.dropped(AT, &[]).unwrap(), Dropped::NotAsked);
    assert_eq!(drained(&e), (String::new(), Vec::new()));
}

/// A program that asks for drops and accepts the drag.
fn accepting(mimes: &[String]) -> GhosttyEngine {
    let mut e = engine();
    e.write(b"\x1b]72;t=a;text/uri-list text/plain\x1b\\");
    let _told = drained(&e);
    assert!(e.drag_over(AT, mimes).unwrap());
    let (told, _) = drained(&e);
    assert!(told.starts_with("\x1b]72;t=m:x=3:y=2:X=28:Y=40:o=1"), "{told:?}");
    e.write(b"\x1b]72;t=m:o=1;text/uri-list text/plain\x1b\\");
    assert_eq!(
        drained(&e).1,
        [EngineEvent::DropAccepted {
            operation: DropOperation::Copy,
            mimes: vec![URIS.to_owned(), "text/plain".to_owned()]
        }]
    );
    e
}

/// The whole drop: the text was pushed before the program asked and answers at once; the
/// files are wanted when asked for and answer once the upload gives them; then it concludes.
#[test]
fn a_drop_answers_what_is_here_and_wants_the_rest() {
    let mimes = [URIS.to_owned(), "text/plain".to_owned()];
    let mut e = accepting(&mimes);
    assert_eq!(e.dropped(AT, &mimes).unwrap(), Dropped::Given);
    assert!(drained(&e).0.starts_with("\x1b]72;t=M:x=3:y=2"));
    e.drop_data(1, Some(b"a.png".to_vec())).unwrap();
    assert_eq!(drained(&e), (String::new(), Vec::new()), "held until asked");

    e.write(b"\x1b]72;t=r:x=2\x1b\\");
    // "a.png"
    assert!(drained(&e).0.contains("t=r:x=2:m=0;YS5wbmc="));
    e.write(b"\x1b]72;t=r:x=1\x1b\\");
    assert_eq!(drained(&e), (String::new(), vec![EngineEvent::DropWants { index: 0 }]));
    e.write(b"\x1b]72;t=r:x=1\x1b\\");
    assert_eq!(drained(&e), (String::new(), Vec::new()), "wanted once");
    e.drop_data(0, Some(b"file:///tmp/drop/a.png\r\n".to_vec())).unwrap();
    // "file:///tmp/drop/a.png\r\n", answering both requests
    let (told, _) = drained(&e);
    assert_eq!(told.matches("t=r:x=1:m=0;ZmlsZTovLy90bXAvZHJvcC9hLnBuZw0K").count(), 2, "{told:?}");

    e.write(b"\x1b]72;t=r:o=1\x1b\\");
    assert_eq!(drained(&e).1, [EngineEvent::DropConcluded { operation: DropOperation::Copy }]);
    e.write(b"\x1b]72;t=A\x1b\\");
    assert_eq!(drained(&e).1, [EngineEvent::DropTarget { accepts: false }]);
}

/// A type fetched as a stream goes straight into the answer to the waiting request, and one
/// that streams in before it is asked for is held whole for it.
#[test]
fn a_streamed_type_answers_as_it_arrives() {
    let mimes = [URIS.to_owned(), "text/plain".to_owned()];
    let mut e = accepting(&mimes);
    assert_eq!(e.dropped(AT, &mimes).unwrap(), Dropped::Given);
    let _told = drained(&e);

    e.write(b"\x1b]72;t=r:x=2\x1b\\");
    assert_eq!(drained(&e).1, [EngineEvent::DropWants { index: 1 }]);
    assert_eq!(e.drop_chunk(1, b"a.").unwrap(), Streamed::Answered);
    // "a."
    let (told, _) = drained(&e);
    assert!(told == "\x1b]72;t=r:x=2:m=0;YS4=\x1b\\", "streams at once: {told:?}");
    e.drop_chunk(1, b"png").unwrap();
    e.drop_end(1, true).unwrap();
    let (told, _) = drained(&e);
    // "png", then the end
    assert_eq!(told, "\x1b]72;t=r:x=2:m=0;cG5n\x1b\\\x1b]72;t=r:x=2\x1b\\");

    assert_eq!(e.drop_chunk(0, b"file:///a\r\n").unwrap(), Streamed::Held);
    e.drop_end(0, true).unwrap();
    assert_eq!(drained(&e), (String::new(), Vec::new()), "held until asked");
    assert_eq!(e.drop_chunk(0, b"late").unwrap(), Streamed::Unwanted, "here whole already");
    e.write(b"\x1b]72;t=r:x=1\x1b\\");
    // "file:///a\r\n"
    assert!(drained(&e).0.contains("t=r:x=1:m=0;ZmlsZTovLy9hDQo="));
}

/// Files that never arrive fail the request rather than leave the program waiting, and so
/// does a stream cut short.
#[test]
fn a_failed_fetch_fails_the_request() {
    let mimes = [URIS.to_owned(), "text/plain".to_owned()];
    let mut e = accepting(&mimes);
    assert_eq!(e.dropped(AT, &mimes).unwrap(), Dropped::Given);
    e.write(b"\x1b]72;t=r:x=1\x1b\\");
    let _told = drained(&e);
    e.drop_data(0, None).unwrap();
    let (told, _) = drained(&e);
    assert!(told.contains("t=R:x=1") && told.contains("EIO"), "{told:?}");

    e.write(b"\x1b]72;t=r:x=2\x1b\\");
    let _told = drained(&e);
    e.drop_chunk(1, b"a").unwrap();
    e.drop_end(1, false).unwrap();
    let (told, _) = drained(&e);
    assert!(told.contains("t=R:x=2") && told.contains("EIO"), "{told:?}");
}

/// A type the drop does not have is not found.
#[test]
fn a_request_past_the_list_is_not_found() {
    let mimes = [URIS.to_owned()];
    let mut e = accepting(&mimes);
    assert_eq!(e.dropped(AT, &mimes).unwrap(), Dropped::Given);
    e.write(b"\x1b]72;t=r:x=5\x1b\\");
    let (told, events) = drained(&e);
    assert!(told.contains("t=R:x=5") && told.contains("ENOENT"), "{told:?}");
    assert!(events.is_empty(), "{events:?}");
}

/// A drop the program never accepted is refused, as kitty refuses one: the program hears the
/// drag leave, and the drop concludes as nothing.
#[test]
fn an_unaccepted_drop_is_refused() {
    let mut e = engine();
    e.write(b"\x1b]72;t=a;text/uri-list\x1b\\");
    let _told = drained(&e);
    let mimes = [URIS.to_owned()];
    assert!(e.drag_over(AT, &mimes).unwrap());
    let _told = drained(&e);
    assert_eq!(e.dropped(AT, &mimes).unwrap(), Dropped::Refused);
    let (told, events) = drained(&e);
    assert!(!told.contains("t=M"), "{told:?}");
    assert_eq!(events, [EngineEvent::DropConcluded { operation: DropOperation::None }]);

    assert!(e.drag_over(AT, &mimes).unwrap());
    let _told = drained(&e);
    e.write(b"\x1b]72;t=m:o=0\x1b\\");
    let _told = drained(&e);
    assert_eq!(e.dropped(AT, &mimes).unwrap(), Dropped::Refused, "an answer of none");
}
