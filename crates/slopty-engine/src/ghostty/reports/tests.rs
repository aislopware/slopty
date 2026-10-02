use slopty_proto::input::CellMetrics;
use slopty_proto::terminal::TermSize;

use super::super::ClipboardSource;
use crate::{EngineConfig, EngineEvent, GhosttyEngine};

fn size(cols: u16, rows: u16) -> TermSize {
    TermSize { cols, rows, metrics: CellMetrics { cell_width: 8, cell_height: 16 } }
}

fn engine() -> GhosttyEngine {
    GhosttyEngine::new(EngineConfig { size: size(20, 4), scrollback_lines: 100 }).unwrap()
}

/// What the engine wrote back to the program for `query`.
fn answer(e: &mut GhosttyEngine, query: &[u8]) -> String {
    e.write(query);
    told(e)
}

fn told(e: &GhosttyEngine) -> String {
    e.drain_events()
        .into_iter()
        .filter_map(|ev| match ev {
            EngineEvent::PtyWrite(b) => Some(String::from_utf8_lossy(&b).into_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn xtversion_names_slopty_and_its_version() {
    let mut e = engine();
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(answer(&mut e, b"\x1b[>q"), format!("\x1bP>|Slopty {version}\x1b\\"));
}

/// The size in cells, in pixels and of a cell, as the driver last set it.
#[test]
fn the_size_queries_are_answered_from_the_drivers_size() {
    let mut e = engine();
    assert_eq!(answer(&mut e, b"\x1b[18t"), "\x1b[8;4;20t");
    assert_eq!(answer(&mut e, b"\x1b[14t"), "\x1b[4;64;160t");
    assert_eq!(answer(&mut e, b"\x1b[16t"), "\x1b[6;16;8t");
    e.resize(TermSize { metrics: CellMetrics { cell_width: 9, cell_height: 18 }, ..size(30, 5) })
        .unwrap();
    let _resized = told(&e);
    assert_eq!(answer(&mut e, b"\x1b[18t"), "\x1b[8;5;30t");
    assert_eq!(answer(&mut e, b"\x1b[16t"), "\x1b[6;18;9t");
}

/// A program that asks for in-band size reports (mode 2048) is told the size at once and again
/// on every resize.
#[test]
fn in_band_size_reports_come_on_asking_and_on_each_resize() {
    let mut e = engine();
    assert_eq!(answer(&mut e, b"\x1b[?2048h"), "\x1b[48;4;20;64;160t");
    e.resize(size(30, 5)).unwrap();
    assert_eq!(told(&e), "\x1b[48;5;30;80;240t");
}

/// A source with no viewer sharing its clipboard.
struct Unshared;

impl ClipboardSource for Unshared {
    fn text(&self) -> Option<String> {
        None
    }

    fn shared(&self) -> bool {
        false
    }
}

/// The primary device attributes list clipboard access (52) only while a viewer shares its
/// clipboard, so a program reads the clipboard only when its reads are answered.
#[test]
fn the_primary_attributes_list_the_clipboard_while_it_is_shared() {
    let mut e = engine();
    assert_eq!(answer(&mut e, b"\x1b[c"), "\x1b[?62;22c");
    e.share_clipboard(Some(Box::new(Unshared)));
    assert_eq!(answer(&mut e, b"\x1b[c"), "\x1b[?62;22c");
    e.share_clipboard(Some(Box::new(|| Some("shared".to_owned()))));
    assert_eq!(answer(&mut e, b"\x1b[c"), "\x1b[?62;22;52c");
    assert_eq!(answer(&mut e, b"\x1b[>c"), "\x1b[>1;0;0c");
}
