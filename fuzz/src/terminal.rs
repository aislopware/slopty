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
//! into a fresh engine of the same size shows the same lines and cursor, holds the same modes,
//! encodes the same keys and mouse reports, and reports the same session state (the title, the
//! directory, the program's colours, the pointer shape and the progress). The replay is then fed
//! what the subject is fed, and at the next checkpoint it must still show what the subject shows
//! (from a checkpoint taken at the parser's ground state, as the worker takes them, and with no
//! resize since, whose reflow tells a space the replay wrote from a cell never written):
//! the state a checkpoint carries without showing it (the saved cursor, the kitty keyboard stack,
//! protected cells, a pending wrap) shows there.
//!
//! A replayed checkpoint is held to each line's cells as they draw, its semantic prompt mark
//! (OSC 133), its hyperlinks (OSC 8) and its soft wrap. Only what draws nothing may differ: a
//! cell never written comes back as a space, and a space's pen can differ where it shows
//! nothing (a foreground colour, bold or blink).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use slopty_engine::{EngineConfig, EngineEvent, GhosttyEngine};
use slopty_grid::{
    Cell, CellText, CellWidth, Color, Hyperlink, Line, LineFlags, SemanticMark, StyleFlags,
    Underline,
};
use slopty_proto::codec;
use slopty_proto::input::{
    CellMetrics, KeyAction, KeyCode, KeyEvent, Mods, MouseAction, MouseButton, MouseEvent,
};
use slopty_proto::terminal::{ColorOverrides, Frame, PointerShape, Progress, TermSize};

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
    let mut session = Session::default();
    let mut replica: Option<GhosttyEngine> = None;
    while let Some((&op, rest)) = script.split_first() {
        script = rest;
        match op {
            0x00..=0xef => {
                let len = usize::from(op % 64).saturating_add(1).min(script.len());
                let (bytes, rest) = script.split_at(len);
                script = rest;
                subject.write(bytes);
                mirror.write(bytes);
                session.fold(subject.drain_events());
                let _events = mirror.drain_events();
                if let Some(replica) = &mut replica {
                    replica.write(bytes);
                    let _events = replica.drain_events();
                }
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
                // A replay writes a blank cell as a space, which draws the same but reflows
                // differently, so a replay that went through a resize is no longer compared.
                replica = None;
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
                replayed(&mut subject, &mut mirror, &mut replica, &session, size);
            }
        }
    }
}

/// A checkpoint of `subject` replayed into a fresh engine shows what `mirror` shows, holds its
/// modes, encodes input as it does and reports `session`; the replay of the last checkpoint,
/// fed everything since, still shows what `mirror` shows. Not during a synchronized update: the
/// checkpoint carries the mode, so the replay holds its screen too.
fn replayed(
    subject: &mut GhosttyEngine,
    mirror: &mut GhosttyEngine,
    replica: &mut Option<GhosttyEngine>,
    session: &Session,
    size: TermSize,
) {
    if subject.hold_remaining().is_some() {
        return;
    }
    let truth = mirror.full_frame(0).expect("the mirror's frame");
    if let Some(mut went_on) = replica.take() {
        let again = went_on.full_frame(0).expect("the replica's frame");
        // Not its marks: a cursor with prompt content on a row the prompt never flagged is
        // replayed writing output when no other row is flagged to give it on, since no OSC 133
        // step gives a cursor prompt content without flagging its row (the engine's
        // `cursor_content`), and a row wrapped into later takes its flag from the content.
        same_screen(&unmarked(&again), &unmarked(&truth), "a replay fed what followed it");
    }
    let mut state = Vec::new();
    subject.checkpoint(&mut state).expect("a checkpoint");
    let Some(mut fresh) = engine(size) else { return };
    fresh.write(&state);
    let mut replayed = Session::default();
    replayed.fold(fresh.drain_events());
    let again = fresh.full_frame(0).expect("the replay's frame");
    same_screen(&again, &truth, "the checkpoint");
    assert_eq!(again.modes, truth.modes, "the checkpoint's modes");
    assert_eq!(&replayed, session, "the checkpoint's session state");
    assert_eq!(inputs(&mut fresh), inputs(subject), "the checkpoint's input encoding");
    // A sequence the subject is still in the middle of has no bytes in a checkpoint, so what
    // follows reads differently in the replay. The worker checkpoints at the ground state.
    if subject.at_ground().expect("the parser's state") {
        *replica = Some(fresh);
    }
}

/// `frame` with every line's semantic mark taken off.
fn unmarked(frame: &Frame) -> Frame {
    let mut frame = frame.clone();
    for update in &mut frame.updates {
        std::sync::Arc::make_mut(&mut update.line).mark = SemanticMark::Unknown;
    }
    frame
}

/// `again`'s lines and cursor are `truth`'s, as they draw.
fn same_screen(again: &Frame, truth: &Frame, what: &str) {
    let (got, want) = (drawn_lines(again), drawn_lines(truth));
    assert_eq!(got.len(), want.len(), "{what}: the rows");
    let differ: Vec<_> = got.iter().zip(&want).enumerate().filter(|(_, (g, w))| g != w).collect();
    assert!(differ.is_empty(), "{what}: the screen, (row, (replayed, truth)): {differ:#?}");
    assert_eq!(again.cursor, truth.cursor, "{what}: the cursor");
}

/// The state a session keeps from its engine's events, as the worker does: the last of each.
#[derive(Debug, Default, PartialEq)]
struct Session {
    title: Option<String>,
    cwd: Option<String>,
    colors: ColorOverrides,
    pointer: PointerShape,
    progress: Progress,
}

impl Session {
    fn fold(&mut self, events: Vec<EngineEvent>) {
        for event in events {
            match event {
                // An empty title is no title: a checkpoint writes none.
                EngineEvent::Title(title) => self.title = Some(title).filter(|t| !t.is_empty()),
                EngineEvent::Cwd(cwd) => self.cwd = Some(cwd),
                EngineEvent::Colors(colors) => self.colors = colors,
                EngineEvent::Pointer(pointer) => self.pointer = pointer,
                EngineEvent::Progress(progress) => self.progress = progress,
                _ => {}
            }
        }
    }
}

/// What a few keys and a click encode as: the modes and the kitty keyboard flags that shape
/// input, seen from outside.
fn inputs(engine: &mut GhosttyEngine) -> Vec<Vec<u8>> {
    let key = |code, text: Option<&str>, mods| KeyEvent {
        seq: 1,
        action: KeyAction::Press,
        code,
        mods,
        consumed_mods: Mods::empty(),
        text: text.map(str::to_owned),
        unshifted: None,
        composing: false,
        option_as_alt: false,
    };
    let keys = [
        key(KeyCode::A, Some("a"), Mods::CTRL),
        key(KeyCode::Enter, Some("\r"), Mods::SHIFT),
        key(KeyCode::Escape, None, Mods::empty()),
        key(KeyCode::ArrowUp, None, Mods::empty()),
        key(KeyCode::Numpad1, Some("1"), Mods::empty()),
        key(KeyCode::Backspace, None, Mods::empty()),
    ];
    let mut out: Vec<Vec<u8>> = keys
        .iter()
        .map(|k| {
            let mut bytes = Vec::new();
            engine.encode_key(k, &mut bytes).expect("a key encodes");
            bytes
        })
        .collect();
    let click = |action| MouseEvent {
        action,
        button: Some(MouseButton::Left),
        mods: Mods::empty(),
        col: 0,
        row: 0,
        px: 2,
        py: 2,
    };
    let mut mouse = Vec::new();
    for action in [MouseAction::Press, MouseAction::Release] {
        engine.encode_mouse(&click(action), &mut mouse).expect("a click encodes");
    }
    out.push(mouse);
    let mut paste = Vec::new();
    engine.encode_paste("p", &mut paste).expect("a paste encodes");
    out.push(paste);
    let mut focus = Vec::new();
    engine.encode_focus(true, &mut focus).expect("a focus encodes");
    out.push(focus);
    out
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

/// A frame's lines as they draw: what a replay must give back.
fn drawn_lines(frame: &Frame) -> Vec<Drawn> {
    frame
        .updates
        .iter()
        .map(|u| Drawn {
            cells: u.line.cells.iter().map(drawn).collect(),
            mark: u.line.mark,
            links: u.line.links.clone(),
            wrapped: u.line.flags.contains(LineFlags::WRAPPED),
        })
        .collect()
}

/// A line as it draws: its cells (a blank that shows nothing, a cell never written or a space
/// whose style marks nothing a space shows, no background, inverse, underline, strike or
/// overline, is [`Cell::BLANK`], and a cell holding only a background is a space in it), its
/// prompt mark, its links and its soft wrap.
#[derive(PartialEq)]
struct Drawn {
    cells: Vec<Cell>,
    mark: SemanticMark,
    links: Vec<Hyperlink>,
    wrapped: bool,
}

/// One line of text, a styled cell as `[text style]`, then what the line carries.
impl std::fmt::Debug for Drawn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut text = String::new();
        for cell in &self.cells {
            let shown = if cell.text.as_str().is_empty() { "·" } else { cell.text.as_str() };
            if cell.style == slopty_grid::Style::default() && cell.width == CellWidth::Narrow {
                text.push_str(shown);
            } else {
                write!(text, "[{shown} {:?} {:?}]", cell.width, cell.style)?;
            }
        }
        f.debug_struct("Drawn")
            .field("text", &text)
            .field("mark", &self.mark)
            .field("links", &self.links)
            .field("wrapped", &self.wrapped)
            .finish()
    }
}

fn drawn(cell: &Cell) -> Cell {
    let shown = StyleFlags::INVERSE | StyleFlags::STRIKETHROUGH | StyleFlags::OVERLINE;
    let style = cell.style;
    let empty = matches!(cell.text.as_str(), "" | " ") && cell.width == CellWidth::Narrow;
    let plain = style.underline == Underline::None && !style.flags.intersects(shown);
    if empty && plain && style.bg == Color::Default {
        Cell::BLANK
    } else if empty && plain {
        // The replay writes a cell an erase left a background in as a space in it.
        Cell { text: CellText::from_char(' '), ..cell.clone() }
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
