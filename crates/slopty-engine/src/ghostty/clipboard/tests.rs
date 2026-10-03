use std::cell::Cell;
use std::rc::Rc;

use slopty_proto::input::CellMetrics;
use slopty_proto::terminal::{MAX_OSC52_BYTES, TermSize};

use super::*;
use crate::{EngineConfig, EngineEvent};

fn engine() -> GhosttyEngine {
    GhosttyEngine::new(EngineConfig {
        size: TermSize {
            cols: 20,
            rows: 4,
            metrics: CellMetrics { cell_width: 8, cell_height: 16 },
        },
        scrollback_lines: 100,
    })
    .unwrap()
}

/// What the engine wrote back to the program for `query`.
fn answer(e: &mut GhosttyEngine, query: &[u8]) -> String {
    e.write(query);
    e.drain_events()
        .into_iter()
        .filter_map(|ev| match ev {
            EngineEvent::PtyWrite(b) => Some(String::from_utf8_lossy(&b).into_owned()),
            _ => None,
        })
        .collect()
}

const READ: &[u8] = b"\x1b]52;c;?\x1b\\";

#[test]
fn a_read_with_nothing_shared_is_answered_empty() {
    let mut e = engine();
    assert_eq!(answer(&mut e, READ), "\x1b]52;c;\x1b\\");
}

#[test]
fn a_read_is_answered_from_the_source_each_time() {
    let mut e = engine();
    let text = Rc::new(RefCell::new(Some("hello".to_owned())));
    let asked = Rc::new(Cell::new(0_u32));
    let (from, count) = (Rc::clone(&text), Rc::clone(&asked));
    e.share_clipboard(Some(Box::new(move || {
        count.set(count.get().saturating_add(1));
        from.borrow().clone()
    })));
    assert_eq!(answer(&mut e, READ), "\x1b]52;c;aGVsbG8=\x1b\\");
    *text.borrow_mut() = None;
    assert_eq!(answer(&mut e, READ), "\x1b]52;c;\x1b\\");
    assert_eq!(asked.get(), 2);
    e.share_clipboard(None);
    assert_eq!(answer(&mut e, READ), "\x1b]52;c;\x1b\\");
}

/// The primary selection is not shared, and text past what a program may copy is not answered.
#[test]
fn only_the_standard_clipboard_within_bounds_is_read() {
    let mut e = engine();
    e.share_clipboard(Some(Box::new(|| Some("hello".to_owned()))));
    assert_eq!(answer(&mut e, b"\x1b]52;p;?\x1b\\"), "\x1b]52;p;\x1b\\");
    e.share_clipboard(Some(Box::new(|| Some("x".repeat(MAX_OSC52_BYTES.saturating_add(1))))));
    assert_eq!(answer(&mut e, READ), "\x1b]52;c;\x1b\\");
}

/// A Kitty clipboard read of text gets it with the listing; one of a type nobody shares is
/// refused.
#[test]
fn a_kitty_read_gets_text_and_nothing_else() {
    let mut e = engine();
    e.share_clipboard(Some(Box::new(|| Some("hello".to_owned()))));
    // "text/plain ."
    let got = answer(&mut e, b"\x1b]5522;type=read:id=r1;dGV4dC9wbGFpbiAu\x1b\\");
    assert!(got.contains("status=OK:id=r1"), "{got:?}");
    assert!(got.contains("mime=dGV4dC9wbGFpbg==;aGVsbG8="), "{got:?}");
    // "image/png"
    let got = answer(&mut e, b"\x1b]5522;type=read:id=r2;aW1hZ2UvcG5n\x1b\\");
    assert!(got.contains("status=EPERM:id=r2"), "{got:?}");
}

/// The one-time password of the paste event in `told`.
fn password(told: &str) -> String {
    let start = told.find(":pw=").unwrap().saturating_add(4);
    let rest = told.get(start..).unwrap();
    rest.get(..rest.find(['\x1b', ';', ':']).unwrap()).unwrap().to_owned()
}

/// A program that turned paste events on is told of a paste as its types, text first, and
/// reads what it wants with the password, once. Whatever it reads after is the shared
/// clipboard's, which is nothing here.
#[test]
fn a_paste_to_a_program_that_asked_is_an_event_it_reads_once() {
    let mut e = engine();
    assert!(!e.paste_events().unwrap());
    assert_eq!(answer(&mut e, b"\x1b[?5522h"), "");
    assert!(e.paste_events().unwrap());
    let html = PasteRep { mime: "text/html", data: b"<b>hi</b>".to_vec() };
    assert!(e.paste_event("hi", vec![html]).unwrap());
    let told: String = e
        .drain_events()
        .into_iter()
        .filter_map(|ev| match ev {
            EngineEvent::PtyWrite(b) => Some(String::from_utf8_lossy(&b).into_owned()),
            _ => None,
        })
        .collect();
    let pw = password(&told);
    // "text/plain text/html\n"
    assert!(
        told.contains("mime=Lg==") && told.contains("dGV4dC9wbGFpbiB0ZXh0L2h0bWwK"),
        "{told:?}"
    );
    // "text/html", read by a program named "app" with the paste's password.
    let read = format!("\x1b]5522;type=read:id=r1:name=YXBw:pw={pw};dGV4dC9odG1s\x1b\\");
    let got = answer(&mut e, read.as_bytes());
    // "<b>hi</b>"
    assert!(got.contains("mime=dGV4dC9odG1s;PGI+aGk8L2I+"), "{got:?}");
    assert!(!got.contains("aGk="), "the text was not asked for: {got:?}");
    let again = answer(&mut e, read.as_bytes());
    assert!(again.contains("status=EPERM"), "{again:?}");
}

/// Nothing to paste tells the program nothing.
#[test]
fn an_empty_paste_event_tells_nothing() {
    let mut e = engine();
    let _on = answer(&mut e, b"\x1b[?5522h");
    assert!(!e.paste_event("", Vec::new()).unwrap());
    assert_eq!(e.drain_events(), Vec::<EngineEvent>::new());
}
