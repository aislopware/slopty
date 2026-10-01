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
