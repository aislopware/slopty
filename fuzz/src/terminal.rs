//! A program's output through the worker's terminal engine.
//!
//! These are the bytes any program in a session writes, a remote host's included, which the VT
//! parser (libghostty-vt) takes and the engine turns into the frames viewers are sent.
//!
//! The first two bytes pick the size; the rest is a script of operations, each led by a byte:
//! - `0x00..=0xef`: write the next `op % 64 + 1` bytes as output, then take a frame;
//! - `0xf0..=0xf3`: resize to the size the next two bytes pick;
//! - `0xf4..=0xf7`: a viewer joins;
//! - `0xf8..=0xfb`: read a page of scrollback, from the line the next byte picks;
//! - `0xfc..=0xff`: checkpoint, and replay the checkpoint into a fresh engine.
//!
//! What must hold, beside no panic in the engine and no trap in the parser (built `ReleaseSafe`
//! by `cargo xtask fuzz`): every viewer, applying the frames it was sent by absolute line index,
//! shows what a second engine fed the same bytes shows whole; every frame comes back from the
//! wire as it went; a scrollback page holds no more lines than asked; and a checkpoint replayed
//! into a fresh engine of the same size shows the same cells and cursor.
//!
//! A replayed checkpoint is held to its cells as they draw, not to all a line carries: a cell
//! never written comes back as a space, and a blank's pen can differ where it draws nothing
//! (a foreground colour, bold or blink on a space). Two things do not come back at all,
//! the lines' semantic prompt marks (OSC 133) and their hyperlinks (OSC 8). Those two are open
//! findings (`docs/decisions/testing.md`, "Terminal output is fuzzed through the engine").

use std::collections::BTreeMap;

use slopty_engine::{EngineConfig, GhosttyEngine};
use slopty_grid::{Cell, CellWidth, Color, Line, StyleFlags, Underline};
use slopty_proto::codec;
use slopty_proto::input::CellMetrics;
use slopty_proto::terminal::{Frame, TermSize};

/// Scrollback the engines keep: small, so a script reaches its eviction.
const SCROLLBACK: u32 = 64;

/// `capture` as a script, as `cargo xtask fuzz` seeds this target.
///
/// An 80 × 24 screen, the capture written in pieces of at most 64 bytes, then a viewer joining,
/// a checkpoint, a resize to 40 × 12 and a checkpoint there.
#[must_use]
pub fn script(capture: &[u8]) -> Vec<u8> {
    let mut script = vec![79, 23];
    for piece in capture.chunks(64) {
        script.push(u8::try_from(piece.len().saturating_sub(1)).unwrap_or(63));
        script.extend_from_slice(piece);
    }
    script.extend_from_slice(&[0xf4, 0xfc, 0xf0, 39, 11, 0xfc]);
    script
}

/// Run one input.
pub fn run(data: &[u8]) {
    let Some((&[cols, rows], mut script)) = data.split_first_chunk::<2>() else { return };
    let size = pick_size(cols, rows);
    let (Some(mut subject), Some(mut mirror)) = (engine(size), engine(size)) else { return };
    let mut first = Viewer::default();
    first.apply(&round_trip(&subject.full_frame(0).expect("a first frame")));
    let mut viewers = vec![first];
    let _first = mirror.full_frame(0).expect("the mirror's first frame");
    while let Some((&op, rest)) = script.split_first() {
        script = rest;
        match op {
            0x00..=0xef => {
                let len = usize::from(op % 64).saturating_add(1).min(script.len());
                let (bytes, rest) = script.split_at(len);
                script = rest;
                subject.write(bytes);
                mirror.write(bytes);
                let _events = (subject.drain_events(), mirror.drain_events());
                let frame = subject.take_frame(0).expect("a frame");
                let Some(frame) = frame else { continue };
                let frame = round_trip(&frame);
                for viewer in &mut viewers {
                    viewer.apply(&frame);
                }
                // A synchronized update (mode 2026) holds the screen the program left until it
                // ends: what the viewers show then is that screen, not the one being drawn.
                if subject.hold_remaining().is_none() {
                    let truth = truth(&mirror.full_frame(0).expect("the mirror's frame"));
                    for (n, viewer) in viewers.iter().enumerate() {
                        assert_eq!(viewer.screen(&frame), truth, "viewer {n}");
                    }
                }
            }
            0xf0..=0xf3 => {
                let Some((&[cols, rows], rest)) = script.split_first_chunk::<2>() else { return };
                script = rest;
                let size = pick_size(cols, rows);
                subject.resize(size).expect("a resize");
                mirror.resize(size).expect("the mirror's resize");
                // A resize is sent whole to everyone.
                let frame = round_trip(&subject.full_frame(0).expect("a full frame"));
                for viewer in &mut viewers {
                    viewer.apply(&frame);
                }
            }
            0xf4..=0xf7 => {
                // What changed since the last frame is the others'; it goes out first.
                if let Some(frame) = subject.take_frame(0).expect("a frame") {
                    let frame = round_trip(&frame);
                    for viewer in &mut viewers {
                        viewer.apply(&frame);
                    }
                }
                let (frame, _images) = subject.join_frame(0).expect("a join");
                let mut joiner = Viewer::default();
                joiner.apply(&round_trip(&frame));
                viewers.push(joiner);
            }
            0xf8..=0xfb => {
                let Some((&start, rest)) = script.split_first() else { return };
                script = rest;
                let count = u32::from(op % 4).saturating_mul(8).saturating_add(1);
                let (_, lines) = subject
                    .lines(slopty_grid::LineIndex(u64::from(start)), count)
                    .expect("a scrollback page");
                let asked = usize::try_from(count).unwrap_or(usize::MAX);
                assert!(lines.len() <= asked, "{} lines for a page of {asked}", lines.len());
            }
            0xfc..=0xff => {
                let size = subject.size();
                replayed(&mut subject, &mut mirror, size);
            }
        }
    }
}

/// A checkpoint of `subject` replayed into a fresh engine shows what `mirror` shows. Not during
/// a synchronized update: the checkpoint carries the mode, so the replay holds its screen too.
fn replayed(subject: &mut GhosttyEngine, mirror: &mut GhosttyEngine, size: TermSize) {
    if subject.hold_remaining().is_some() {
        return;
    }
    let mut state = Vec::new();
    subject.checkpoint(&mut state).expect("a checkpoint");
    let Some(mut fresh) = engine(size) else { return };
    fresh.write(&state);
    let (again, truth) = (
        fresh.full_frame(0).expect("the replay's frame"),
        mirror.full_frame(0).expect("the mirror's frame"),
    );
    assert_eq!(cells(&again), cells(&truth), "the checkpoint's screen");
    assert_eq!(again.cursor, truth.cursor, "the checkpoint's cursor");
}

/// Two bytes as a size small enough to run fast, never zero.
fn pick_size(cols: u8, rows: u8) -> TermSize {
    TermSize {
        cols: u16::from(cols % 80).saturating_add(1),
        rows: u16::from(rows % 24).saturating_add(1),
        metrics: CellMetrics { cell_width: 8, cell_height: 16 },
    }
}

fn engine(size: TermSize) -> Option<GhosttyEngine> {
    GhosttyEngine::new(EngineConfig { size, scrollback_lines: SCROLLBACK }).ok()
}

/// The frame as a viewer decodes it off the wire, which must be the frame sent.
fn round_trip(frame: &Frame) -> Frame {
    let body = codec::encode_body(frame).expect("a frame encodes");
    let back: Frame = codec::decode_body(&body).expect("a frame decodes");
    assert_eq!(&back, frame, "the wire changed a frame");
    back
}

fn lines(frame: &Frame) -> Vec<String> {
    frame.updates.iter().map(|u| format!("{:?}", u.line)).collect()
}

/// A frame's cells, row by row, as they draw: a blank that shows nothing (a cell never written,
/// or a space whose style marks nothing a space shows, no background, inverse, underline,
/// strike or overline) is [`Cell::BLANK`].
fn cells(frame: &Frame) -> Vec<Vec<Cell>> {
    frame.updates.iter().map(|u| u.line.cells.iter().map(drawn).collect()).collect()
}

fn drawn(cell: &Cell) -> Cell {
    let shown = StyleFlags::INVERSE | StyleFlags::STRIKETHROUGH | StyleFlags::OVERLINE;
    let style = cell.style;
    let empty = matches!(cell.text.as_str(), "" | " ") && cell.width == CellWidth::Narrow;
    if empty
        && style.bg == Color::Default
        && style.underline == Underline::None
        && !style.flags.intersects(shown)
    {
        Cell::BLANK
    } else {
        cell.clone()
    }
}

/// A full frame's rows, as [`Viewer::screen`] shows them.
fn truth(frame: &Frame) -> Vec<Option<String>> {
    assert!(frame.full, "a full frame");
    lines(frame).into_iter().map(Some).collect()
}

/// A viewer: every line it was sent, by numbering and absolute index, as a client keeps them.
#[derive(Default)]
struct Viewer {
    held: BTreeMap<(u32, u64), Line>,
}

impl Viewer {
    fn apply(&mut self, frame: &Frame) {
        let first = frame.first_visible_line.0;
        for u in &frame.updates {
            self.held.insert(
                (frame.epoch, first.saturating_add(u64::from(u.row))),
                Line::clone(&u.line),
            );
        }
    }

    /// The screen this viewer shows after `frame`.
    fn screen(&self, frame: &Frame) -> Vec<Option<String>> {
        (0..u64::from(frame.rows))
            .map(|y| {
                self.held
                    .get(&(frame.epoch, frame.first_visible_line.0.saturating_add(y)))
                    .map(|l| format!("{l:?}"))
            })
            .collect()
    }
}
