//! The terminal engine, backed by libghostty-vt.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

pub use clipboard::{ClipboardSource, PasteRep, TEXT_MIME};
pub use dnd::{DropOperation, DropPoint, Dropped, Streamed};
use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
use libghostty_vt::kitty::graphics::{self as kitty_graphics, PlacementIterator};
use libghostty_vt::render::{CellIterator, Dirty, RenderState, RowIteration, RowIterator};
use libghostty_vt::screen::{
    Cell as VtCell, CellContentTag, CellFields, CellLayout, CellSemanticContent, GridRef,
    RowSemanticPrompt, Screen as VtScreen, TrackedGridRef,
};
use libghostty_vt::style::{PaletteIndex, RgbColor};
use libghostty_vt::terminal::{
    ClipboardLocation, ColorScheme, Mode, Point, PointCoordinate, PointSpace,
    ProgressState as VtProgress,
};
use libghostty_vt::{Terminal, focus, key, mouse, paste};
pub use memory::{Compression, Memory};
use slopty_core::{Duration, MonoTime};
use slopty_grid::{
    Cell, CellText, CellWidth, Cursor, Hyperlink, Line, LineFlags, LineIndex, RowUpdate,
    SemanticMark, Style, TermModes,
};
use slopty_proto::input::{
    KeyAction, KeyCode, KeyEvent, Mods, MouseAction, MouseButton, MouseEvent,
};
use slopty_proto::terminal::{
    BlockMark, Blocks, ColorOverrides, Frame, LineDiscipline, MAX_ABOVE, MAX_OSC52_BYTES,
    PixelRect, Placement, PointerShape, Progress, ProgressState, TermColors, TermSize,
};

use crate::graphics::{self, ImageUpload, Ledger, Shipped};
use crate::placeholder::{self, Runs};
use crate::{EngineConfig, EngineError, EngineEvent, convert, osc133, search};

mod carried;
mod clipboard;
mod dnd;
mod memory;
mod read;
mod redraw;
mod reports;
mod restored;
#[cfg(test)]
mod session_state;

pub use read::{CommandBlock, Position, ScreenText, TextLines, TextSince};

/// Longest title or body of a desktop notification passed on: a banner shows a line or two,
/// and a program can write anything into an OSC.
const NOTIFICATION_CHARS: usize = 512;

/// How long a program may hold synchronized output (mode 2026) before the engine ends the hold
/// itself: libghostty has no clock, so a program that never releases one would freeze its
/// screen.
const SYNC_OUTPUT_TIMEOUT: Duration = Duration::from_millis(1000);

/// A prompt row takes the exit status of a `133;D` this many rows above it at most: the shell
/// may print a blank line or a partial-line marker between the mark and the prompt.
const EXIT_LOOKBACK_ROWS: u64 = 4;

type Events = Rc<RefCell<Vec<EngineEvent>>>;

/// A render hold (synchronized output, mode 2026) in progress. The render state holds the
/// frame the program left on screen when it began, and frames come from that until it ends.
#[derive(Clone, Copy, Debug)]
struct Hold {
    since: MonoTime,
    /// History rows when it began, which the captured frame's rows are numbered against.
    scrollback: u64,
}

/// Who a frame is for, which decides what building it consumes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Take {
    /// The changes since the last frame, for every viewer.
    Diff,
    /// Every row, for every viewer (a resize): the next sequence number, all images again.
    Everyone,
    /// Every row, for a viewer joining: the current sequence number, and the dirty state the
    /// other viewers' next diff needs is left as it is.
    Joiner,
}

/// libghostty-vt engine. `!Send`: lives on the session thread that owns the PTY reader.
pub struct GhosttyEngine {
    // Dropped before `term` (declaration order): it holds a pointer to the terminal.
    anchor: Option<Anchor>,
    primary_anchor: Option<Anchor>,
    term: Terminal<'static, 'static>,
    /// libghostty's activity token when a pass compressing the history last finished: nothing
    /// is left to compress until it moves, or until a read of the history restores pages
    /// (which does not move it).
    compressed_at: std::cell::Cell<Option<libghostty_vt::terminal::CompressionActivity>>,
    /// Shared with the render-hold callback, which captures a frame into it.
    render: Rc<RefCell<RenderState<'static>>>,
    hold: Rc<std::cell::Cell<Option<Hold>>>,
    rows_iter: RowIterator<'static>,
    cells_iter: CellIterator<'static>,
    key_enc: key::Encoder<'static>,
    key_ev: key::Event<'static>,
    mouse_enc: mouse::Encoder<'static>,
    mouse_ev: mouse::Event<'static>,
    events: Events,
    /// Whether the driver's background is light: what `CSI ? 996 n` is answered with, shared
    /// with the callback.
    light: Rc<std::cell::Cell<bool>>,
    size: TermSize,
    seq: u64,
    /// The numbering in force.
    epoch: u32,
    /// The last numbering handed out: numberings are never reused, so a client can tell the
    /// primary's coming back from a new one.
    epochs: u32,
    /// The primary screen's numbering and marks while the alternate screen is up, as
    /// `primary_anchor`.
    primary_marks: Option<PrimaryMarks>,
    /// What the viewers following the diffs hold, so a scroll ships only the rows whose
    /// content changed.
    shown: Shown,
    /// Absolute index of screen row 0 of the active screen.
    base: u64,
    on_alt: bool,
    /// The primary screen as VT bytes, taken just before the program switched to the alternate
    /// screen, so a checkpoint made on the alternate screen can carry both.
    primary_snapshot: Option<Vec<u8>>,
    /// Counts the writes, resizes and colour changes, so work over the whole terminal (the
    /// primary snapshot, search's plain text) is redone only when something changed.
    generation: u64,
    /// The `generation` `primary_snapshot` was taken at.
    primary_at: u64,
    /// Search's text of the rows that scrolled into history, and the last needle's hits.
    history: search::History,
    /// A write since the last colour check carried an OSC or a reset, and the next write
    /// checks again in case the sequence was split across the two.
    colours_touched: bool,
    /// The tail of the last chunk when it ended inside a possible alternate-screen switch
    /// (`ESC [ ? 10`), so a switch split across two reads is still seen before it completes.
    alt_prefix: Vec<u8>,
    /// The buttons whose press the program was told of and whose release it was not, one bit
    /// per button: a motion report under 1002 carries whether any is down.
    buttons_down: u8,
    /// The pty's line discipline as the worker last read it (`None` until it does): whether
    /// the kernel echoes what is typed, and whether input is line-buffered.
    line_discipline: Option<LineDiscipline>,
    /// The line discipline changed since the last frame: the next one goes out to say so,
    /// though no cell moved.
    discipline_changed: bool,
    /// The caret the last frame to every viewer carried. libghostty dirties no row for a caret
    /// that changes in place (DECSCUSR, DECTCEM), so a change is a frame of its own.
    cursor_sent: Option<Cursor>,
    scratch: String,
    /// A row read and found the same as the one the viewers hold, kept for the next row to be
    /// read into: a scroll re-reads every row and ships only the few that came in, and a
    /// fresh line per row was most of what a frame allocated (`tests/allocs.rs`).
    spare_line: Option<Line>,
    /// The record's list of rows before the last frame replaced it, emptied, for the next
    /// frame's record to be built in.
    spare_rows: Vec<Option<Arc<Line>>>,
    /// The same for the record's prints.
    spare_prints: Prints,
    /// Scratch for OSC 8 URIs (`ghostty_grid_ref_hyperlink_uri` wants a caller buffer).
    uri_buf: Vec<u8>,
    /// The prompt marks and full resets libghostty reported during the write in progress, in
    /// stream order; the write takes them once it has settled.
    marks: Rc<RefCell<Vec<Pending>>>,
    /// The last write's marks, taken and emptied, for the next write's to be swapped into.
    spare_marks: Vec<Pending>,
    /// Exit status reported on an absolute line (the row the cursor was on at the `D`).
    exit_marks: BTreeMap<u64, Option<u8>>,
    /// Absolute lines a primary prompt started on (`133;A`).
    prompt_starts: BTreeSet<u64>,
    /// Rows the next frame carries whether or not a cell changed: a `133;C` moved the cursor
    /// row out of the prompt without writing to it, and the client's command tracking waits
    /// on that row (a silent `sleep` was not seen running until its output or its end).
    forced_rows: BTreeSet<u64>,
    /// Rows whose prompt mark changed with the marks alone: a `D` or an `A` on a row above a
    /// prompt gives it a status or takes it, which dirties no row in libghostty. The next
    /// frame reads them again and sends those the viewers hold otherwise.
    remarked_rows: BTreeSet<u64>,
    /// The OSC 133 command blocks of the active screen, oldest first (see [`read`]).
    commands: std::collections::VecDeque<read::Block>,
    /// The primary screen's blocks while the alternate screen is up, as `primary_anchor`.
    primary_commands: std::collections::VecDeque<read::Block>,
    /// `133;D` marks seen since the engine started, for a waiter on the next command's end.
    commands_ended: u64,
    /// Walks the kitty graphics placements of the active screen.
    placements: PlacementIterator<'static>,
    /// The images the clients hold.
    ledger: Ledger,
    /// Images to send ahead of the frames just taken.
    uploads: Vec<ImageUpload>,
    /// The graphics storage's generation at the last frame: a change alone makes a frame.
    graphics_gen: u64,
    /// The placements the last frame's placeholder runs made, as their line was then.
    screen_runs: Vec<Placement>,
    /// Placeholder runs that scrolled above the screen, oldest line first: their cells are in
    /// the history, which no frame scans, so they are kept as each client keeps them.
    runs_above: Vec<Placement>,
    /// The command blocks that started (their output did) or ended since the last frame the
    /// viewers took.
    block_news: Vec<BlockMark>,
    /// The program's colour changes as last reported.
    overrides: ColorOverrides,
    /// The program's progress report as last reported, shared with libghostty's callback.
    progress: Rc<std::cell::Cell<Progress>>,
    /// The title and the directory as last reported, shared with libghostty's callbacks.
    reported: Rc<RefCell<Reported>>,
    /// The pointer shape the program asked for (`OSC 22`) as last reported.
    pointer: PointerShape,
    /// What the program's clipboard reads are answered from, shared with libghostty's callback.
    clipboard: clipboard::Shared,
    /// The drop a program reads through the Kitty drag and drop protocol, and what it did.
    drops: dnd::Shared,
    /// Nothing was written yet: a checkpoint written first brings its marks back (see
    /// [`carried`]).
    fresh: bool,
    /// A checkpoint is being replayed: the marks its screens carry are taken from its payloads,
    /// and the OSC 133 marks the formatter wrote into them are not counted again.
    restoring: bool,
}

/// The primary screen's numbering, parked while a program has the alternate screen.
#[derive(Debug)]
struct PrimaryMarks {
    epoch: u32,
    exit_marks: BTreeMap<u64, Option<u8>>,
    prompt_starts: BTreeSet<u64>,
}

/// The rows the viewers that follow the diffs hold, as the last diff (or resize) left them:
/// the absolute line of the top row and each row's line, `None` where not known.
///
/// libghostty rebuilds every row when the viewport moves, and the viewport follows the
/// output, so every line scrolled in made a full frame: an Enter at a bottom prompt sent the
/// whole screen (88 kB at 200 × 60). With this a scroll ships only the rows whose line is
/// not already held at its absolute index; the client moves the rest up itself. The lines
/// themselves are kept, not a hash of them: comparing costs less than hashing every cell
/// did (MEASUREMENTS 2026-09-25, "a scroll ships the rows it moved"), and it is exact.
///
/// A row is held as the allocation the frame carried, so the record costs no copy: once the
/// frame is encoded and dropped the record is its only holder, and the next change of that
/// row is read into it in place.
#[derive(Debug, Default)]
struct Shown {
    epoch: u32,
    cols: u16,
    first: u64,
    rows: Vec<Option<Arc<Line>>>,
    /// What each held line was read from, index for index with `rows`.
    prints: Prints,
    /// A held row had kitty placeholder cells when it was read. Their images are placed from
    /// every row, changed or not, so the next frame walks every row.
    placeholders: bool,
}

/// A row's flags that reach its line: a wrap continuation, and its prompt state.
type RowFlags = (bool, RowSemanticPrompt);

/// What the held lines were read from: each row's raw cells, all rows in one allocation, and
/// the two row flags that reach the line. libghostty marks every row dirty when the screen
/// scrolls, so a frame after one line of output re-reads the whole screen; a row that reads as
/// its print holds the line it was built into, which is kept without building it again or
/// comparing it cell by cell. A row whose raw cells do not say everything (links, clusters,
/// placeholders, a mark the engine forced) has no flags, so it never matches.
#[derive(Debug, Default)]
struct Prints {
    cells: Vec<VtCell>,
    /// Per row: where its cells start in `cells`, and its flags.
    rows: Vec<(usize, Option<RowFlags>)>,
}

impl Prints {
    fn clear(&mut self) {
        self.cells.clear();
        self.rows.clear();
    }

    /// Record row `i`: over its own print when the record is kept in place, as the next row
    /// otherwise.
    fn record(
        &mut self,
        i: usize,
        in_place: bool,
        cells: impl ExactSizeIterator<Item = VtCell>,
        flags: Option<RowFlags>,
    ) {
        if !in_place {
            self.push(cells, flags);
            return;
        }
        let Some(&(start, _)) = self.rows.get(i) else { return };
        let end = self.rows.get(i.saturating_add(1)).map_or(self.cells.len(), |&(next, _)| next);
        match self.cells.get_mut(start..end) {
            Some(slot) if slot.len() == cells.len() => {
                for (dst, src) in slot.iter_mut().zip(cells) {
                    *dst = src;
                }
                if let Some(row) = self.rows.get_mut(i) {
                    row.1 = flags;
                }
            }
            // A row of another width cannot be matched; keep its cells, drop its flags.
            _ => {
                if let Some(row) = self.rows.get_mut(i) {
                    row.1 = None;
                }
            }
        }
    }

    /// Record the next row: its raw cells and, when it may be matched later, its flags.
    fn push(&mut self, cells: impl Iterator<Item = VtCell>, flags: Option<RowFlags>) {
        self.rows.push((self.cells.len(), flags));
        self.cells.extend(cells);
    }

    /// Row `i`'s cells and flags.
    fn row(&self, i: usize) -> Option<(&[VtCell], Option<RowFlags>)> {
        let &(start, flags) = self.rows.get(i)?;
        let end = self.rows.get(i.checked_add(1)?).map_or(self.cells.len(), |&(next, _)| next);
        Some((self.cells.get(start..end)?, flags))
    }

    /// Row `i`'s cells and flags, when it has flags.
    fn get(&self, i: usize) -> Option<(&[VtCell], RowFlags)> {
        let (cells, flags) = self.row(i)?;
        Some((cells, flags?))
    }
}

impl Shown {
    /// Whether it describes a screen of this numbering and size.
    fn holds(&self, epoch: u32, cols: u16, rows: u16) -> bool {
        !self.rows.is_empty()
            && self.epoch == epoch
            && self.cols == cols
            && self.rows.len() == usize::from(rows)
    }

    fn slot(&mut self, line: u64) -> Option<&mut Option<Arc<Line>>> {
        let i = usize::try_from(line.checked_sub(self.first)?).ok()?;
        self.rows.get_mut(i)
    }

    /// The line held at absolute `line`.
    fn at(&self, line: u64) -> Option<&Arc<Line>> {
        let i = usize::try_from(line.checked_sub(self.first)?).ok()?;
        self.rows.get(i)?.as_ref()
    }

    /// The line held at absolute `line`, moved out for the next frame's record.
    fn take(&mut self, line: u64) -> Option<Arc<Line>> {
        self.slot(line)?.take()
    }

    /// What the line held at absolute `line` was read from, flags or not.
    fn print_row(&self, line: u64) -> Option<(&[VtCell], Option<RowFlags>)> {
        let i = usize::try_from(line.checked_sub(self.first)?).ok()?;
        self.prints.row(i)
    }

    /// What the line held at absolute `line` was read from.
    fn print_at(&self, line: u64) -> Option<(&[VtCell], RowFlags)> {
        let i = usize::try_from(line.checked_sub(self.first)?).ok()?;
        self.prints.get(i)
    }

    /// Move the held lines of screen rows `rows` (the top one at absolute `first`) into the
    /// next frame's record, as rows no frame read again. Kept out of the frame's row loop,
    /// whose every other row pays for its size.
    #[inline(never)]
    fn carry(&mut self, into: &mut Vec<Option<Arc<Line>>>, first: u64, rows: std::ops::Range<u16>) {
        for row in rows {
            into.push(self.take(first.saturating_add(u64::from(row))));
        }
    }

    fn forget(&mut self, line: u64) {
        if let Some(slot) = self.slot(line) {
            *slot = None;
        }
    }
}

/// A tracked row plus its absolute index.
struct Anchor {
    tracked: TrackedGridRef,
    abs: u64,
}

/// What libghostty reported during a write that the engine takes once the write has settled.
enum Pending {
    /// A prompt mark, with the cursor row where the shell wrote it tracked through whatever
    /// the rest of the write scrolled or evicted.
    Mark { mark: osc133::Mark, row: Option<TrackedGridRef>, col: u16, screen: VtScreen },
    /// A full reset (RIS): the screen, the history and the alternate screen are gone.
    Reset,
}

impl std::fmt::Debug for GhosttyEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GhosttyEngine")
            .field("size", &self.size)
            .field("seq", &self.seq)
            .field("epoch", &self.epoch)
            .field("base", &self.base)
            .field("on_alt", &self.on_alt)
            .finish_non_exhaustive()
    }
}

impl GhosttyEngine {
    /// Create an engine.
    pub fn new(config: EngineConfig) -> Result<Self, EngineError> {
        check_size(config.size)?;
        let mut term = Terminal::new(config.size.cols, config.size.rows)?;
        term.resize(
            config.size.cols,
            config.size.rows,
            u32::from(config.size.metrics.cell_width),
            u32::from(config.size.metrics.cell_height),
        )?;
        // libghostty-vt also caps scrollback by *bytes*, and its default is 10 KB: with only
        // the line limit raised a session kept about one page (~900 rows) of history. The line
        // limit is the contract; lift the byte cap so it governs.
        term.set_scrollback_max_bytes(None)?;
        term.set_scrollback_max_lines(Some(config.scrollback_lines as usize))?;
        // Kitty graphics: a storage limit turns the protocol on; the decoder is per thread,
        // and the engine lives on its session's thread.
        term.set_kitty_image_storage_limit(graphics::KITTY_STORAGE_BYTES)?;
        graphics::allow_media(&mut term)?;
        // A kitty clipboard write (OSC 5522) is buffered whole before the callback sees it, up
        // to 64 MiB by default; nothing past the session's ceiling would be passed on anyway.
        term.set_clipboard_write_max_bytes(Some(MAX_OSC52_BYTES))?;
        // Grapheme clustering (mode 2027) on from the start and after a reset, as Ghostty,
        // Kitty and WezTerm have it: an emoji sequence or a flag is one wide cell, not a cell
        // per code point. A program can still turn it off.
        term.set_default_mode(Mode::GRAPHEME_CLUSTER, true)?;
        kitty_graphics::set_png_decoder(Some(Box::new(graphics::PngDecoder)))?;
        let dark = slopty_theme::TerminalPalette::DARK.wire();
        set_colors(&mut term, &dark)?;
        let light = Rc::new(std::cell::Cell::new(is_light(dark.bg)));

        let events: Events = Rc::new(RefCell::new(Vec::new()));
        let reported = Rc::new(RefCell::new(Reported::default()));
        install_callbacks(&mut term, &events, &light, &reported)?;
        let progress = Rc::new(std::cell::Cell::new(Progress::default()));
        install_progress(&mut term, &events, &progress)?;
        let render = Rc::new(RefCell::new(RenderState::new()?));
        let hold = Rc::new(std::cell::Cell::new(None));
        install_render_hold(&mut term, &render, &hold)?;
        let marks = Rc::new(RefCell::new(Vec::new()));
        install_marks(&mut term, &marks)?;
        let clipboard = Rc::new(RefCell::new(clipboard::Reads::default()));
        clipboard::install(&mut term, &clipboard)?;
        let drops = dnd::install(&mut term)?;
        reports::install(&mut term, &clipboard)?;

        let mut engine = Self {
            anchor: None,
            primary_anchor: None,
            term,
            render,
            hold,
            rows_iter: RowIterator::new()?,
            compressed_at: std::cell::Cell::new(None),
            cells_iter: CellIterator::new()?,
            key_enc: key::Encoder::new()?,
            key_ev: key::Event::new()?,
            mouse_enc: mouse::Encoder::new()?,
            mouse_ev: mouse::Event::new()?,
            events,
            light,
            size: config.size,
            seq: 0,
            epoch: 0,
            epochs: 0,
            primary_marks: None,
            shown: Shown::default(),
            base: 0,
            on_alt: false,
            primary_snapshot: None,
            generation: 0,
            primary_at: u64::MAX,
            history: search::History::default(),
            colours_touched: false,
            alt_prefix: Vec::new(),
            buttons_down: 0,
            line_discipline: None,
            discipline_changed: false,
            cursor_sent: None,
            scratch: String::with_capacity(16),
            spare_line: None,
            spare_rows: Vec::new(),
            spare_prints: Prints::default(),
            uri_buf: vec![0; 256],
            marks,
            spare_marks: Vec::new(),
            exit_marks: BTreeMap::new(),
            prompt_starts: BTreeSet::new(),
            forced_rows: BTreeSet::new(),
            remarked_rows: BTreeSet::new(),
            commands: std::collections::VecDeque::new(),
            primary_commands: std::collections::VecDeque::new(),
            commands_ended: 0,
            placements: PlacementIterator::new()?,
            ledger: Ledger::default(),
            uploads: Vec::new(),
            graphics_gen: 0,
            screen_runs: Vec::new(),
            runs_above: Vec::new(),
            block_news: Vec::new(),
            overrides: ColorOverrides::default(),
            progress,
            reported,
            pointer: PointerShape::Text,
            clipboard,
            drops,
            fresh: true,
            restoring: false,
        };
        engine.reanchor()?;
        Ok(engine)
    }

    /// Every retained row (history then screen) as plain text, one line per row with trailing
    /// blanks trimmed; blank rows at the very end are omitted.
    #[cfg(test)]
    fn plain_text(&self) -> Result<String, EngineError> {
        let total = u32::try_from(self.total_rows()?).unwrap_or(u32::MAX);
        self.plain_rows(0, total.saturating_sub(1))
    }

    /// The whole terminal as the VT byte stream that rebuilds it in a fresh engine of the same
    /// size: palette, modes, tab stops, scrolling region, keyboard state (the kitty keyboard
    /// stack whole), every retained row (history then screen, soft wraps kept, each row's prompt
    /// flag, each cell's semantic content and protection), and the cursor (its position with a
    /// pending wrap, shape, pen, hyperlink, semantic content and the state `DECSC` saved), then
    /// the prompt marks and command blocks the engine keeps, and last the session's title,
    /// working directory (OSC 7), pointer shape and progress. libghostty-vt's own formatter
    /// writes the screens, so what a program drew comes back exactly as its cells, not as an
    /// approximation from the grid.
    ///
    /// When the alternate screen is active the formatter can only see that screen, so the bytes
    /// are the primary screen as of the moment the program switched (kept by [`Self::write`])
    /// followed by the alternate screen; a program that leaves the alternate screen after the
    /// replay finds its primary where it was.
    ///
    /// The bytes are appended to `out`.
    ///
    /// # Errors
    ///
    /// When the formatter fails.
    pub fn checkpoint(&mut self, out: &mut Vec<u8>) -> Result<(), EngineError> {
        self.screens(out)?;
        self.session_tail(out);
        Ok(())
    }

    /// The program's state no screen holds, which the session tells every viewer: the title,
    /// the directory, the pointer shape and the progress report. It follows the screens, so no
    /// prompt mark replayed after it drops the progress.
    ///
    /// The directory is the one last reported rather than libghostty's: a full reset clears
    /// libghostty's and the shell is still where it was, and a value `OSC 7` gave that is no
    /// directory was never the session's.
    fn session_tail(&self, out: &mut Vec<u8>) {
        if let Ok(title) = self.term.title()
            && !title.is_empty()
        {
            out.extend_from_slice(b"\x1b]2;");
            out.extend_from_slice(title.as_bytes());
            out.extend_from_slice(b"\x1b\\");
        }
        let reported = self.reported.borrow();
        if !reported.pwd.is_empty() {
            out.extend_from_slice(b"\x1b]7;");
            out.extend_from_slice(reported.pwd.as_bytes());
            out.extend_from_slice(b"\x1b\\");
        }
        if self.pointer != PointerShape::default() {
            let name = convert::pointer_name(self.pointer);
            out.extend_from_slice(format!("\x1b]22;{name}\x1b\\").as_bytes());
        }
        let progress = self.progress.get();
        let state = match progress.state {
            ProgressState::None => return,
            ProgressState::Set => 1,
            ProgressState::Error => 2,
            ProgressState::Indeterminate => 3,
            ProgressState::Paused => 4,
        };
        let report = match progress.percent {
            Some(percent) => format!("\x1b]9;4;{state};{percent}\x1b\\"),
            None => format!("\x1b]9;4;{state}\x1b\\"),
        };
        out.extend_from_slice(report.as_bytes());
    }

    /// The screens of a checkpoint: the active one, or the primary as it was when the program
    /// switched followed by the alternate screen.
    fn screens(&mut self, out: &mut Vec<u8>) -> Result<(), EngineError> {
        if !self.on_alt {
            let active = self.primary_now()?;
            out.extend_from_slice(active);
            return Ok(());
        }
        let active = self.format_active_screen()?;
        let primary = self.primary_snapshot.as_deref().unwrap_or_default();
        out.reserve(primary.len().saturating_add(active.len()).saturating_add(64));
        out.extend_from_slice(primary);
        // Each blob only sets the modes that differ from the defaults, so a mode the primary
        // had on and the program turned off on the alternate screen would stay on: put every
        // mode the primary set back to its default before the alternate screen sets its own.
        out.extend_from_slice(&mode_resets(primary));
        // Enter the alternate screen here, saving the primary cursor the snapshot just placed,
        // and home: the formatter writes content from wherever the cursor is, and it is where
        // the primary left it. Its own `?1049h` (in the modes it emits) is then a no-op.
        out.extend_from_slice(b"\x1b[?1049h");
        // The cursor's shape is the terminal's, and the primary's blob set the one it had then:
        // give the alternate screen the default back for its own blob to set its shape over.
        out.extend_from_slice(b"\x1b[0 q");
        // The scrolling region is the terminal's, not a screen's: the one the primary's blob
        // set would scroll the alternate screen's rows away as they are written. Its own blob
        // sets it again after them; the left and right margins went with `?69` above.
        out.extend_from_slice(b"\x1b[r");
        // The formatter writes a screen as if into a fresh terminal and sets the cursor's pen,
        // hyperlink, protection, character sets and semantic content after it. The primary's
        // were set at the end of its blob, and the switch carries the cursor and the character
        // sets over: put them back to the defaults on the alternate screen, which leaves the
        // primary's saved for the way back.
        out.extend_from_slice(FRESH_PEN);
        out.extend_from_slice(FRESH_CHARSETS);
        out.extend_from_slice(b"\x1b[H");
        out.extend_from_slice(&active);
        Ok(())
    }

    /// The primary screen as VT bytes now, formatted again only if something changed since
    /// the last time. Also the fallback for an alternate-screen switch [`Self::feed`] fails to
    /// see: at worst the primary comes back as of the last checkpoint.
    fn primary_now(&mut self) -> Result<&[u8], EngineError> {
        if self.primary_at != self.generation || self.primary_snapshot.is_none() {
            self.primary_snapshot = Some(self.format_active_screen()?);
            self.primary_at = self.generation;
        }
        Ok(self.primary_snapshot.as_deref().unwrap_or_default())
    }

    /// The active screen and the terminal state around it, as VT bytes.
    ///
    /// The formatter writes every row, the blank ones at the bottom too, so a primary screen
    /// that has scrolled comes back with all of its history, and leaves a soft-wrapped row
    /// for the replay to wrap. It writes the cursor last, after the margins: its position
    /// (relative to them under origin mode, with a pending wrap kept), its shape, and the state
    /// `DECSC` saved. Then come the content the cursor writes with, and the marks the engine
    /// keeps (see [`carried`]).
    fn format_active_screen(&self) -> Result<Vec<u8>, EngineError> {
        let options = FormatterOptions::new()
            .with_format(Format::Vt)
            .with_unwrap(true)
            .with_trim(false)
            .with_trailing_rows(true)
            // Not the palette: the formatter writes all 256 entries as OSC 4 sets, which would
            // come back as the program's changes over whatever the next driver paints with.
            // The program's own colour changes are written below.
            .with_palette(false)
            .with_modes(true)
            .with_scrolling_region(true)
            // The stops a program set (`tabs 4`), then home: `CSI 3 g`, and `CSI n G` and
            // `ESC H` for each stop.
            .with_tabstops(true)
            // The directory goes in the session's state, after the screens.
            .with_pwd(false)
            .with_keyboard(true)
            .with_cursor(true)
            .with_style(true)
            .with_hyperlink(true)
            .with_protection(true)
            .with_kitty_keyboard(true)
            .with_charsets(true)
            // Every row's prompt flag and every cell's semantic content (OSC 133); the marks
            // the engine keeps of them follow in the payload.
            .with_semantic_prompt(true);
        let mut formatter = Formatter::new(&self.term, options)?;
        let bytes = formatter.format_alloc(None)?;
        let mut out = colour_sets(&self.overrides);
        out.reserve(bytes.len().saturating_add(128));
        out.extend_from_slice(&bytes);
        out.extend_from_slice(self.cursor_content()?);
        let marks = carried::Payload {
            base: self.base,
            exit_marks: &self.exit_marks,
            prompt_starts: &self.prompt_starts,
            commands: &self.commands,
        };
        out.extend_from_slice(marks.to_string().as_bytes());
        Ok(out)
    }

    /// The OSC 133 step that gives the cursor back the content it writes with: the formatter
    /// leaves it in whatever its last row ended in. Output is `D`, which unlike `C` never
    /// takes a row's prompt flag off. Prompt content flags the row it starts on, so it is given
    /// back only on a row flagged that way already; on any other the cursor writes output
    /// rather than mark a row the program never marked.
    fn cursor_content(&self) -> Result<&'static [u8], EngineError> {
        const OUTPUT: &[u8] = b"\x1b]133;D\x1b\\";
        Ok(match self.term.cursor_semantic_content()? {
            CellSemanticContent::Output => OUTPUT,
            CellSemanticContent::Input if self.term.cursor_semantic_clear_eol()? => {
                b"\x1b]133;I\x1b\\"
            }
            CellSemanticContent::Input => b"\x1b]133;B\x1b\\",
            CellSemanticContent::Prompt => {
                let at = Point::Active(PointCoordinate {
                    x: self.term.cursor_x()?,
                    y: u32::from(self.term.cursor_y()?),
                });
                match self.term.grid_ref(at)?.row()?.semantic_prompt()? {
                    RowSemanticPrompt::Prompt => b"\x1b]133;P;k=i\x1b\\",
                    RowSemanticPrompt::Continuation => b"\x1b]133;P;k=c\x1b\\",
                    RowSemanticPrompt::None => OUTPUT,
                }
            }
        })
    }

    /// Absolute index one past the newest line (history + screen).
    fn total_lines(&self) -> Result<u64, EngineError> {
        Ok(self.base.saturating_add(self.term.total_rows()? as u64))
    }

    fn total_rows(&self) -> Result<u64, EngineError> {
        Ok(self.term.total_rows()? as u64)
    }

    /// Pin the anchor to the newest active row and record its absolute index.
    fn reanchor(&mut self) -> Result<(), EngineError> {
        let total = self.total_rows()?;
        let y = self.size.rows.saturating_sub(1);
        let tracked =
            self.term.track_grid_ref(Point::Active(PointCoordinate { x: 0, y: u32::from(y) }))?;
        self.anchor =
            Some(Anchor { tracked, abs: self.base.saturating_add(total).saturating_sub(1) });
        Ok(())
    }

    fn bump_epoch(&mut self) {
        let open = self.open_command();
        self.epochs = self.epochs.wrapping_add(1);
        self.epoch = self.epochs;
        self.base = 0;
        self.exit_marks.clear();
        self.prompt_starts.clear();
        self.forced_rows.clear();
        self.remarked_rows.clear();
        self.commands.clear();
        self.block_news.clear();
        self.screen_runs.clear();
        self.runs_above.clear();
        if let Some(open) = open {
            let history = self.term.scrollback_rows().map_or(0, |n| n as u64);
            let y = self.term.cursor_y().map_or(0, u64::from);
            self.reopen_command(history.saturating_add(y), open);
        }
        tracing::debug!(epoch = self.epoch, "line numbering invalidated");
    }

    /// Feed one chunk. If the chunk switches to the alternate screen, the primary screen is
    /// snapshotted first (for [`Self::checkpoint`]): the bytes before the switch are fed, the
    /// snapshot taken, then the rest.
    fn feed(&mut self, chunk: &[u8]) {
        if !self.on_alt && !self.alt_prefix.is_empty() {
            // The last chunk ended inside `ESC [ ? 10…`: if this one completes a switch, the
            // terminal is still on the primary (the sequence is not finished), so snapshot now.
            let mut probe = std::mem::take(&mut self.alt_prefix);
            probe.extend(chunk.iter().take(8));
            if alt_enter_at(&probe) == Some(0) {
                self.snapshot_primary();
            }
        }
        let switch_at = if self.on_alt { None } else { alt_enter_at(chunk) };
        let rest = match switch_at {
            Some(at) => {
                let (before, from_switch) = chunk.split_at(at);
                if !before.is_empty() {
                    self.generation = self.generation.wrapping_add(1);
                    self.term.vt_write(before);
                    self.settle_or_bump();
                }
                if !self.on_alt {
                    self.snapshot_primary();
                }
                from_switch
            }
            None => chunk,
        };
        self.generation = self.generation.wrapping_add(1);
        self.term.vt_write(rest);
        self.settle_or_bump();
        if !self.on_alt {
            self.alt_prefix = alt_prefix_of(chunk).to_vec();
        }
    }

    /// Replay a checkpoint into this fresh engine: each screen's bytes, then the marks its
    /// payload carries in place of the ones the replay recorded (see [`carried`]).
    fn restore(&mut self, mut bytes: &[u8]) {
        self.restoring = true;
        while let Some((screen, body, rest)) = carried::split(bytes) {
            self.feed(screen);
            if let Some(marks) = carried::decode(body, self.base) {
                self.exit_marks = marks.exit_marks;
                self.prompt_starts = marks.prompt_starts;
                self.commands = marks.commands;
                self.generation = self.generation.wrapping_add(1);
            } else {
                tracing::warn!("a checkpoint's marks did not read; its screen has none");
            }
            bytes = rest;
        }
        self.restoring = false;
        // The replay's blocks were numbered as it went; the restored ones go out whole.
        self.block_news.clear();
        if !bytes.is_empty() {
            self.feed(bytes);
        }
    }

    /// Keep the primary screen for a checkpoint made on the alternate screen. A program that
    /// starts right after a checkpoint (nothing written since) reuses it rather than formatting
    /// the whole history again on the session's thread.
    fn snapshot_primary(&mut self) {
        if let Err(e) = self.primary_now() {
            tracing::warn!(error = %e, "primary snapshot failed");
            self.primary_snapshot = None;
        }
    }

    fn settle_or_bump(&mut self) {
        if let Err(e) = self.settle() {
            tracing::error!(error = %e, "engine settle failed; invalidating line numbering");
            self.bump_epoch();
        }
        self.take_marks();
    }

    /// Record what libghostty reported during the write just settled, in the order the shell
    /// wrote it.
    #[expect(clippy::iter_with_drain, reason = "the emptied Vec is kept for the next write")]
    fn take_marks(&mut self) {
        let mut taken = std::mem::take(&mut self.spare_marks);
        std::mem::swap(&mut taken, &mut *self.marks.borrow_mut());
        for pending in taken.drain(..) {
            match pending {
                // A checkpoint's screens carry their marks; the formatter's are its cells'.
                Pending::Mark { .. } if self.restoring => {}
                Pending::Mark { mark, row, col, screen } => {
                    // A mark on a screen that is gone again: the alternate screen's marks are
                    // dropped with it, and a switch in from the primary settles before it.
                    if screen != self.active_screen() {
                        continue;
                    }
                    // Its row went out of the history in the same write: counted at the oldest
                    // line, so a command end still ends its command.
                    let y = row.and_then(|r| r.point(PointSpace::Screen).ok().flatten());
                    let line = self.base.saturating_add(y.map_or(0, |p| u64::from(p.y)));
                    self.record_mark(mark, line, col);
                }
                Pending::Reset => self.forget_screen(),
            }
        }
        self.spare_marks = taken;
    }

    const fn active_screen(&self) -> VtScreen {
        if self.on_alt { VtScreen::Alternate } else { VtScreen::Primary }
    }

    /// A full reset (RIS) cleared the screen and the history and left the alternate screen:
    /// every line the numbering, the marks and the command blocks refer to is gone. A new
    /// numbering starts with no marks, and no blocks but a command still running.
    fn forget_screen(&mut self) {
        // libghostty clears the title without a callback for it.
        if std::mem::take(&mut self.reported.borrow_mut().titled) {
            self.events.borrow_mut().push(EngineEvent::Title(String::new()));
        }
        self.primary_anchor = None;
        self.primary_marks = None;
        self.primary_commands.clear();
        self.primary_snapshot = None;
        self.bump_epoch();
        if let Err(e) = self.reanchor() {
            tracing::error!(error = %e, "reanchor after a full reset failed");
        }
    }

    /// The shell wrote a prompt mark on absolute `line` at `col`: remember where, and the
    /// status.
    fn record_mark(&mut self, mark: osc133::Mark, line: u64, col: u16) {
        self.note_command_mark(line, col, mark);
        // Evicted history can never be read again; drop its marks with it.
        let base = self.base;
        if self.exit_marks.first_key_value().is_some_and(|(&l, _)| l < base)
            || self.prompt_starts.first().is_some_and(|&l| l < base)
        {
            let (marks, starts) = (&mut self.exit_marks, &mut self.prompt_starts);
            remark(marks, starts, &mut self.remarked_rows, base, |marks, starts| {
                *marks = marks.split_off(&base);
                *starts = starts.split_off(&base);
            });
        }
        match mark {
            osc133::Mark::PromptStart => {
                // The shell has the terminal back, so whatever reported progress has ended,
                // cleared or not.
                if self.progress.replace(Progress::default()).state != ProgressState::None {
                    self.events.borrow_mut().push(EngineEvent::Progress(Progress::default()));
                }
                let (marks, starts) = (&mut self.exit_marks, &mut self.prompt_starts);
                remark(marks, starts, &mut self.remarked_rows, line, |_, starts| {
                    starts.insert(line);
                });
            }
            osc133::Mark::CommandEnd { exit } => {
                let (marks, starts) = (&mut self.exit_marks, &mut self.prompt_starts);
                remark(marks, starts, &mut self.remarked_rows, line, |marks, _| {
                    marks.insert(line, exit);
                });
            }
            osc133::Mark::OutputStart => {
                self.forced_rows.insert(line);
            }
        }
    }

    /// After output was consumed: follow the anchor to keep `base` exact, handle screen switches.
    fn settle(&mut self) -> Result<(), EngineError> {
        let on_alt = self.term.active_screen()? == VtScreen::Alternate;
        if on_alt != self.on_alt {
            self.on_alt = on_alt;
            if on_alt {
                // Park the primary's numbering with its marks; the alternate screen has no
                // history and a numbering of its own.
                self.primary_anchor = self.anchor.take();
                self.primary_commands = std::mem::take(&mut self.commands);
                self.primary_marks = Some(PrimaryMarks {
                    epoch: self.epoch,
                    exit_marks: std::mem::take(&mut self.exit_marks),
                    prompt_starts: std::mem::take(&mut self.prompt_starts),
                });
                self.bump_epoch();
                self.reanchor()?;
                return Ok(());
            }
            // Back on the primary: its numbering, marks and blocks come back with the parked
            // anchor, so the clients take back the lines they held; a new numbering if the
            // anchor did not survive (checked below).
            self.anchor = self.primary_anchor.take();
            self.commands = std::mem::take(&mut self.primary_commands);
            self.forced_rows.clear();
            self.remarked_rows.clear();
            match self.primary_marks.take() {
                Some(parked) if self.anchor.is_some() => {
                    self.epoch = parked.epoch;
                    self.exit_marks = parked.exit_marks;
                    self.prompt_starts = parked.prompt_starts;
                }
                _ => self.bump_epoch(),
            }
        }

        let followed = match &self.anchor {
            Some(anchor) => anchor
                .tracked
                .point(PointSpace::Screen)?
                .map(|p| (anchor.abs.saturating_sub(u64::from(p.y)), u64::from(p.y))),
            None => None,
        };
        match followed {
            Some((base, y)) => {
                self.base = base;
                // Still on the newest row (nothing scrolled): the anchor stays as it is.
                if y.saturating_add(1) == self.total_rows()? {
                    return Ok(());
                }
            }
            None => self.bump_epoch(),
        }
        self.reanchor()
    }

    fn cursor(snapshot: &libghostty_vt::render::Snapshot<'_, '_>) -> Result<Cursor, EngineError> {
        let cursor = snapshot.cursor()?;
        let (row, col) = cursor.viewport.map_or((0, 0), |c| (c.y, c.x));
        Ok(Cursor {
            row,
            col,
            shape: convert::cursor_shape(cursor.visual_style),
            visible: cursor.visible,
            blink: cursor.blinking,
        })
    }

    fn modes(&self) -> Result<TermModes, EngineError> {
        let t = &self.term;
        let mut m = TermModes::empty();
        m.set(TermModes::ALT_SCREEN, self.on_alt);
        m.set(TermModes::MOUSE_TRACKING, t.is_mouse_tracking()?);
        m.set(TermModes::MOUSE_DRAG, t.mode(Mode::BUTTON_MOUSE)?);
        m.set(TermModes::MOUSE_MOTION, t.mode(Mode::ANY_MOUSE)?);
        if let Some(discipline) = self.line_discipline {
            m.set(TermModes::ECHO_OFF, !discipline.echo);
            m.set(TermModes::CANONICAL, discipline.canonical);
        }
        m.set(TermModes::ALT_SCROLL, t.mode(Mode::ALT_SCROLL)?);
        m.set(TermModes::BRACKETED_PASTE, t.mode(Mode::BRACKETED_PASTE)?);
        m.set(TermModes::FOCUS_EVENTS, t.mode(Mode::FOCUS_EVENT)?);
        let kitty = t.kitty_keyboard_flags()?;
        m.set(TermModes::KITTY_KEYBOARD, !kitty.is_empty());
        m.set(TermModes::KEY_RELEASES, kitty.contains(key::KittyKeyFlags::REPORT_EVENTS));
        m.set(TermModes::SYNC_OUTPUT, t.mode(Mode::SYNC_OUTPUT)?);
        m.set(TermModes::CURSOR_HIDDEN, !t.mode(Mode::CURSOR_VISIBLE)?);
        m.set(TermModes::APP_CURSOR_KEYS, t.mode(Mode::DECCKM)?);
        Ok(m)
    }

    /// The render hold in force, after ending one that lasted past [`SYNC_OUTPUT_TIMEOUT`].
    fn hold_in_force(&mut self) -> Result<Option<Hold>, EngineError> {
        let Some(hold) = self.hold.get() else { return Ok(None) };
        if hold.since.elapsed() < SYNC_OUTPUT_TIMEOUT {
            return Ok(Some(hold));
        }
        // Setting the mode by hand is not reported back, and a program that sets it again
        // cannot push the deadline: a new hold only begins once this one ended.
        self.term.set_mode(Mode::SYNC_OUTPUT, false)?;
        self.hold.set(None);
        Ok(None)
    }

    /// How long until the render hold in force times out, when one is: the session frames
    /// then, whether or not the program writes again.
    #[must_use]
    pub fn hold_remaining(&self) -> Option<std::time::Duration> {
        self.hold
            .get()
            .map(|h| SYNC_OUTPUT_TIMEOUT.to_std().saturating_sub(h.since.elapsed().to_std()))
    }

    fn build_frame(&mut self, input_ack: u64, take: Take) -> Result<Option<Frame>, EngineError> {
        let hold = self.hold_in_force()?;
        let render = Rc::clone(&self.render);
        let mut render = render.borrow_mut();
        // During a hold the render state keeps the frame captured when it began.
        let snapshot =
            if hold.is_some() { render.snapshot()? } else { render.update(&self.term)? };
        let dirty = snapshot.dirty()?;
        let cols = snapshot.cols()?;
        let rows = snapshot.rows()?;
        // Every row is read again; for the viewers that follow the diffs only those whose line
        // they do not hold at its index are sent, unless the numbering or the size changed.
        let rebuild = take != Take::Diff || dirty == Dirty::Full;
        let full = rebuild && !(take == Take::Diff && self.shown.holds(self.epoch, cols, rows));
        // A placement added or deleted moves no cell, but the frame must say so. Rows forced
        // or remarked by a prompt mark are read from the live grid, so they wait for the hold
        // to end.
        let graphics_gen = self.term.kitty_graphics()?.generation()?;
        let forcing = hold.is_none() && !self.forced_rows.is_empty();
        let cursor = Self::cursor(&snapshot)?;
        if !rebuild
            && dirty == Dirty::Clean
            && self.cursor_sent == Some(cursor)
            && (graphics_gen == self.graphics_gen || hold.is_some())
            && (self.block_news.is_empty() || hold.is_some())
            && !forcing
            && (hold.is_some() || self.remarked_rows.is_empty())
            && !self.discipline_changed
        {
            return Ok(None);
        }
        // The placements above the screen go with a frame every viewer takes whole, and with
        // one after the storage changed; otherwise the clients move them up themselves.
        let above_due = take != Take::Diff || full || graphics_gen != self.graphics_gen;
        // Every block goes with a frame every viewer takes whole; otherwise the news.
        let blocks_due = take != Take::Diff || full;
        if take != Take::Joiner && hold.is_none() {
            self.graphics_gen = graphics_gen;
        }

        let scrollback = match hold {
            Some(h) => h.scrollback,
            None => self.term.scrollback_rows()? as u64,
        };
        let first = self.base.saturating_add(scrollback);
        let mut updates = Vec::with_capacity(if full { usize::from(rows) } else { 8 });
        // What the viewers following the diffs hold once this frame is applied.
        let mut shown = std::mem::take(&mut self.spare_rows);
        shown.reserve(usize::from(rows));
        let known = self.shown.holds(self.epoch, cols, rows);
        // Nothing scrolled: the record's prints are the rows' own, and only the rows read again
        // are printed again, in place.
        let in_place = known && take != Take::Joiner && self.shown.first == first;
        let mut prints = if in_place {
            std::mem::take(&mut self.shown.prints)
        } else {
            let mut spare = std::mem::take(&mut self.spare_prints);
            spare.clear();
            spare.rows.reserve(usize::from(rows));
            spare.cells.reserve(usize::from(rows).saturating_mul(usize::from(cols)));
            spare
        };

        // Nothing scrolled and no row is forced or remarked: only the rows libghostty marks
        // dirty are visited, and every other one keeps the line and print the record holds.
        let sparse = in_place
            && !rebuild
            && !forcing
            && (hold.is_some() || self.remarked_rows.is_empty())
            && !self.shown.placeholders;

        let layout = CellLayout::linked();
        // Each row's flag in one call, read in place as the rows are visited.
        let dirty_rows = snapshot.dirty_rows()?;
        let mut row_iter = self.rows_iter.update(&snapshot)?;
        let mut y: u16 = 0;
        // Placeholder cells of virtual kitty placements, gathered from every row (a run that
        // did not change still places its image in this frame).
        let mut runs = Runs::default();
        // The last row visited and whether it soft-wraps into the next.
        let mut above: Option<(u16, bool)> = None;
        // A line's wrap is the row above's (see `row_above_wraps`): the clean rows below a
        // visited row whose wrap changed, and their wrap now.
        let mut rewrapped_below: Vec<(u16, bool)> = Vec::new();
        while let Some((at, row)) = next_row(&mut row_iter, sparse, y) {
            if at != y {
                self.shown.carry(&mut shown, first, y..at);
                y = at;
            }
            let raw = row.raw_row()?;
            // The row flag may be a false positive, but a row without it has no placeholders.
            let placeholders = raw.has_kitty_virtual_placeholder()?;
            let abs = first.saturating_add(u64::from(y));
            let forced = forcing
                && if take == Take::Joiner {
                    self.forced_rows.contains(&abs)
                } else {
                    self.forced_rows.remove(&abs)
                };
            // Looked up row by row: erasing a prompt below remarks the rows under it.
            let remarked = hold.is_none()
                && if take == Take::Joiner {
                    self.remarked_rows.contains(&abs)
                } else {
                    self.remarked_rows.remove(&abs)
                };
            // Last, of a clean row: the row above wraps otherwise than the line the viewers
            // hold says, or the record does not say what they hold.
            let build = rebuild
                || dirty_rows.get(usize::from(y)) == Some(true)
                || forced
                || remarked
                || (!sparse
                    && hold.is_none()
                    && known
                    && above.is_some_and(|(at, wraps)| {
                        at.wrapping_add(1) == y
                            && self
                                .shown
                                .at(abs)
                                .is_none_or(|held| held.flags.contains(LineFlags::WRAPPED) != wraps)
                    }));
            if !build && take != Take::Joiner {
                shown.push(if known { self.shown.take(abs) } else { None });
                // Not dirty: the row reads as it did when its print was taken.
                if !in_place {
                    match self.shown.print_row(abs).filter(|_| known) {
                        Some((cells, flags)) => {
                            prints.rows.push((prints.cells.len(), flags));
                            prints.cells.extend_from_slice(cells);
                        }
                        None => prints.push(row.cells_raw()?, None),
                    }
                }
            }
            // The row flags may be false positives, but a row without one has none of what it
            // names: no links, no styled cell, no multi-codepoint cluster.
            let (row_has_links, styled, clusters) = if build {
                (raw.has_hyperlink()?, raw.is_styled()?, raw.has_grapheme_cluster()?)
            } else {
                (false, false, false)
            };
            let (wrapped, row_semantic) = if build {
                let wrapped = match above {
                    Some((at, wraps)) if at.wrapping_add(1) == y => wraps,
                    // During a hold the live grid is past the rows the frame shows.
                    _ if hold.is_some() => raw.is_wrap_continuation()?,
                    // The row above is clean, so it wraps as it did when this row's line was
                    // last flagged.
                    _ if sparse && let Some(held) = self.shown.at(abs) => {
                        held.flags.contains(LineFlags::WRAPPED)
                    }
                    _ => row_above_wraps(&self.term, y)?,
                };
                (wrapped, raw.semantic_prompt().unwrap_or(RowSemanticPrompt::None))
            } else {
                (false, RowSemanticPrompt::None)
            };
            let wraps = raw.is_wrapped()?;
            above = Some((y, wraps));
            if sparse
                && dirty_rows.get(usize::from(y).saturating_add(1)) == Some(false)
                && self
                    .shown
                    .at(abs.saturating_add(1))
                    .is_none_or(|held| held.flags.contains(LineFlags::WRAPPED) != wraps)
            {
                rewrapped_below.push((y.saturating_add(1), wraps));
            }
            let row_flags =
                (build && !forced && !remarked && !placeholders).then_some((wrapped, row_semantic));
            // The line the viewers hold at this index, when the row still reads as it did when
            // that line was built from it.
            let unchanged = if build && known && !row_has_links && !clusters {
                let held_print =
                    if in_place { prints.get(usize::from(y)) } else { self.shown.print_at(abs) };
                let same_cells = match held_print {
                    Some((cells, flags)) if Some(flags) == row_flags => {
                        cells.iter().copied().eq(row.cells_raw()?)
                    }
                    _ => false,
                };
                match self.shown.at(abs) {
                    Some(held) if same_cells && styled => {
                        // A style id names a style only while a cell uses it: one freed and
                        // taken by another style leaves the cell's bits as they were.
                        let mut it = self.cells_iter.update(row)?;
                        let mut last_id = None;
                        let mut same = true;
                        for (x, rc) in row.cells_raw()?.enumerate() {
                            let f = cell_fields(layout, rc)?;
                            if !f.has_styling() || last_id == Some(f.style_id) {
                                continue;
                            }
                            last_id = Some(f.style_id);
                            it.select(u16::try_from(x).unwrap_or(u16::MAX))?;
                            let style = convert::style(&it.style()?);
                            if held.cells.get(x).is_none_or(|c| c.style != style) {
                                same = false;
                                break;
                            }
                        }
                        same.then(|| Arc::clone(held))
                    }
                    Some(held) if same_cells => Some(Arc::clone(held)),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(held) = unchanged {
                if take == Take::Joiner {
                    updates.push(RowUpdate { row: y, line: held });
                } else {
                    drop(held);
                    let held = self.shown.take(abs);
                    if full && let Some(line) = &held {
                        updates.push(RowUpdate { row: y, line: Arc::clone(line) });
                    }
                    shown.push(held);
                    prints.record(usize::from(y), in_place, row.cells_raw()?, row_flags);
                    row.set_dirty(false)?;
                }
            } else if build {
                let mut line = blank_line(self.spare_line.take(), cols);
                let mut first_semantic = None;
                let mut first_input = None;
                let mut links = LinkRuns::default();
                // The row's cells come in one read, each decoded at once; the cell iteration,
                // positioned per cell, is only for what a raw cell cannot say (a style, a
                // cluster's codepoints).
                let mut cell_iter = if styled || clusters || placeholders {
                    Some(self.cells_iter.update(row)?)
                } else {
                    None
                };
                // Style ids name styles within one page, and a row never spans two.
                let mut last_style = None;
                let mut x: u16 = 0;
                for rc in row.cells_raw()? {
                    let Some(slot) = line.cells.get_mut(usize::from(x)) else { break };
                    let f = cell_fields(layout, rc)?;
                    if first_semantic.is_none() {
                        first_semantic = Some(f.semantic_content);
                    }
                    if first_input.is_none()
                        && matches!(f.semantic_content, CellSemanticContent::Input)
                    {
                        first_input = Some(x);
                    }
                    let width = convert::cell_width(f.wide);
                    let has_style = f.has_styling();
                    // Zero for a blank cell and for one holding only a background colour.
                    let codepoint = if width.draws_text() { f.codepoint } else { 0 };
                    let placeholder = placeholders && codepoint == placeholder::PLACEHOLDER;
                    let cluster = codepoint != 0
                        && matches!(f.content_tag, CellContentTag::CodepointGrapheme);
                    let at = match cell_iter.as_mut() {
                        Some(it) if has_style || cluster || placeholder => {
                            it.select(x)?;
                            Some(&*it)
                        }
                        _ => None,
                    };
                    let style = match at {
                        Some(it) if has_style => {
                            let id = f.style_id;
                            match last_style {
                                Some((last, style)) if last == id => style,
                                _ => {
                                    let style = convert::style(&it.style()?);
                                    last_style = Some((id, style));
                                    style
                                }
                            }
                        }
                        _ => Style::DEFAULT,
                    };
                    let text = match at {
                        Some(it) if placeholder || cluster => {
                            it.graphemes_utf8(&mut self.scratch)?;
                            if placeholder {
                                // The image goes where the placeholder is; the character
                                // itself is never drawn.
                                runs.cell(x, y, placeholder_cell(&style, &self.scratch));
                                CellText::EMPTY
                            } else {
                                runs.finish();
                                CellText::from_cluster(&self.scratch)
                            }
                        }
                        _ => {
                            runs.finish();
                            match char::from_u32(codepoint) {
                                Some(c) if codepoint != 0 => CellText::from_char(c),
                                _ => CellText::EMPTY,
                            }
                        }
                    };
                    if row_has_links {
                        let uri = if f.hyperlink {
                            let gr = self.term.grid_ref(Point::Viewport(PointCoordinate {
                                x,
                                y: u32::from(y),
                            }))?;
                            hyperlink_uri(&gr, &mut self.uri_buf)?
                        } else {
                            None
                        };
                        links.push(x, uri, width == CellWidth::SpacerTail);
                    }
                    *slot = Cell { text, style, width };
                    x = x.saturating_add(1);
                }
                line.links = links.finish(x);
                // A cell holding only a background colour has no style id, and its own row
                // flag: read after the row, since a check in the loop above costs every cell
                // of every row.
                if raw.has_background()? {
                    paint_backgrounds(&mut line, layout, row.cells_raw()?)?;
                }
                line.flags.set(LineFlags::WRAPPED, wrapped);
                // The render state copies a row when the terminal dirtied it; a forced or
                // remarked row was not, so its prompt flag is read from the live grid.
                let semantic = if forced || remarked {
                    self.term
                        .grid_ref(Point::Viewport(PointCoordinate { x: 0, y: u32::from(y) }))?
                        .row()?
                        .semantic_prompt()?
                } else {
                    row_semantic
                };
                // A row erased in place (`CSI 2 J`, ⌃L at a prompt) keeps its number but not
                // its prompt: the marks the shell wrote there are gone with it, else the next
                // prompt drawn below would read as a continuation of a start that no longer
                // exists.
                if semantic != RowSemanticPrompt::Prompt && self.prompt_starts.contains(&abs) {
                    let (marks, starts) = (&mut self.exit_marks, &mut self.prompt_starts);
                    remark(marks, starts, &mut self.remarked_rows, abs, |marks, starts| {
                        starts.remove(&abs);
                        marks.remove(&abs);
                    });
                }
                line.mark = first_semantic.map_or(SemanticMark::Unknown, |first| {
                    convert::semantic_mark(
                        semantic,
                        first,
                        self.prompt_starts.contains(&abs),
                        exit_for(&self.exit_marks, &self.prompt_starts, abs),
                        first_input,
                    )
                });
                if take == Take::Joiner {
                    // The joiner holds this line as it is now, the others as they were sent
                    // it: a line changed and changed back since then is theirs to be sent.
                    let line = match if known { self.shown.at(abs) } else { None } {
                        Some(held) if **held == line => {
                            self.spare_line = Some(line);
                            Arc::clone(held)
                        }
                        Some(_) => {
                            self.shown.forget(abs);
                            Arc::new(line)
                        }
                        None => Arc::new(line),
                    };
                    updates.push(RowUpdate { row: y, line });
                } else {
                    let held = if known { self.shown.take(abs) } else { None };
                    let same = held.as_deref().is_some_and(|l| *l == line);
                    let line = match held {
                        Some(held) if same => {
                            self.spare_line = Some(line);
                            held
                        }
                        Some(mut held) => match Arc::get_mut(&mut held) {
                            // No frame still holds the row the viewers were sent (it was
                            // encoded and dropped): the new line moves into its allocation, and
                            // the old one's cells become the next row's read.
                            Some(slot) => {
                                self.spare_line = Some(std::mem::replace(slot, line));
                                held
                            }
                            None => Arc::new(line),
                        },
                        None => Arc::new(line),
                    };
                    if full || forced || !same {
                        updates.push(RowUpdate { row: y, line: Arc::clone(&line) });
                    }
                    shown.push(Some(line));
                    prints.record(usize::from(y), in_place, row.cells_raw()?, row_flags);
                }
                if take != Take::Joiner {
                    row.set_dirty(false)?;
                }
            } else if placeholders {
                let mut cell_iter = self.cells_iter.update(row)?;
                let mut x: u16 = 0;
                while let Some(cell) = cell_iter.next() {
                    if cell.raw_cell()?.codepoint()? == placeholder::PLACEHOLDER {
                        let style = if cell.has_styling()? {
                            convert::style(&cell.style()?)
                        } else {
                            Style::DEFAULT
                        };
                        cell.graphemes_utf8(&mut self.scratch)?;
                        runs.cell(x, y, placeholder_cell(&style, &self.scratch));
                    } else {
                        runs.finish();
                    }
                    x = x.saturating_add(1);
                }
            }
            runs.finish();
            y = y.saturating_add(1);
        }
        if sparse {
            self.shown.carry(&mut shown, first, y..rows);
        }
        if !rewrapped_below.is_empty() {
            self.rewrap(&rewrapped_below, &mut shown, &mut updates, hold.is_some(), scrollback)?;
        }
        // Every run was gathered from the rows it lies on, and a row visited or not has the
        // placeholder cells it had when the record last read it.
        let placeholders = !runs.runs.is_empty();
        let total = scrollback.saturating_add(u64::from(rows));
        let (blocks, (images, above)) = match take {
            Take::Joiner => {
                if !known {
                    // The record is of another numbering or size, which the joiner never held
                    // (and a viewer catching up holds an older version of): should the
                    // numbering come back, as the primary's does after the alternate screen,
                    // its next diff has to be whole.
                    self.shown = Shown::default();
                }
                // The joiner holds no image, and what it is sent it holds alongside the other
                // viewers: from here on the ledger is those, and whatever else the others hold
                // is shipped again when it is placed again.
                self.ledger.clear();
                self.spare_rows = shown;
                self.spare_prints = prints;
                let blocks = Blocks { whole: true, marks: self.block_marks() };
                (Some(blocks), self.placed_if(graphics_gen, runs, first, above_due)?)
            }
            Take::Everyone | Take::Diff => {
                let record =
                    Shown { epoch: self.epoch, cols, first, rows: shown, prints, placeholders };
                let old = std::mem::replace(&mut self.shown, record);
                let mut spare = old.rows;
                spare.clear();
                self.spare_rows = spare;
                if in_place {
                    // The next scroll copies the record into the spare; have it ready now,
                    // not in that frame.
                    let (cells, rows) =
                        (self.shown.prints.cells.len(), self.shown.prints.rows.len());
                    self.spare_prints.clear();
                    self.spare_prints.cells.reserve(cells);
                    self.spare_prints.rows.reserve(rows);
                } else {
                    self.spare_prints = old.prints;
                }
                if forcing {
                    // A forced row that is no longer on the screen has nothing left to say.
                    self.forced_rows.clear();
                }
                if hold.is_none() {
                    self.remarked_rows.clear();
                }
                snapshot.set_dirty(Dirty::Clean)?;
                self.discipline_changed = false;
                self.cursor_sent = Some(cursor);
                self.seq = self.seq.wrapping_add(1);
                if take == Take::Everyone {
                    // Every viewer takes this frame as a resync, holding nothing yet.
                    self.ledger.clear();
                }
                let blocks = if blocks_due {
                    self.block_news.clear();
                    Some(Blocks { whole: true, marks: self.block_marks() })
                } else if self.block_news.is_empty() || hold.is_some() {
                    None
                } else {
                    Some(Blocks { whole: false, marks: std::mem::take(&mut self.block_news) })
                };
                (blocks, self.placed_if(graphics_gen, runs, first, above_due)?)
            }
        };
        Ok(Some(Frame {
            seq: self.seq,
            full,
            epoch: self.epoch,
            cols,
            rows,
            cursor,
            modes: self.modes()?,
            oldest_line: LineIndex(self.base),
            first_visible_line: LineIndex(first),
            total_lines: self.base.saturating_add(total),
            input_ack,
            updates,
            images,
            above,
            blocks,
        }))
    }

    fn placed_if(
        &mut self,
        graphics_gen: u64,
        runs: Runs,
        first: u64,
        above_due: bool,
    ) -> Result<Placed, EngineError> {
        if graphics_gen == 0 {
            return Ok((Vec::new(), None));
        }
        self.placed(&runs.into_runs(), first, above_due)
    }

    /// Every placement on the viewport, as libghostty lays it out at the client's cell size,
    /// then a placement per run of placeholder cells showing a virtual placement. `first` is the
    /// absolute line of the frame's top row, which the runs were read from.
    ///
    /// With `above_due`, the placements wholly above the viewport, in the history, are listed
    /// too ([`Frame::above`]), the [`MAX_ABOVE`] nearest the screen.
    ///
    /// The pixels of any image the clients do not hold are queued for upload first.
    fn placed(
        &mut self,
        runs: &[placeholder::Run],
        first: u64,
        above_due: bool,
    ) -> Result<Placed, EngineError> {
        let graphics = self.term.kitty_graphics()?;
        // A placement is pinned to the live grid, whose top may be past a held frame's.
        let live_first = self.base.saturating_add(self.term.scrollback_rows()? as u64);
        let mut out = Vec::new();
        let mut above = Vec::new();
        let mut virtuals: Vec<Virtual> = Vec::new();
        let mut it = self.placements.update(&graphics)?;
        while let Some(p) = it.next() {
            let id = p.image_id()?;
            if p.is_virtual()? {
                if !runs.is_empty() {
                    virtuals.push(Virtual {
                        image: id,
                        placement: p.placement_id()?,
                        grid: placeholder::Grid { cols: p.columns()?, rows: p.rows()? },
                        z: p.z()?,
                    });
                }
                continue;
            }
            let Some(image) = graphics.image(id) else { continue };
            let info = p.placement_render_info(&image, &self.term)?;
            // Off the viewport, libghostty's row is still the pin's, unless the placement has
            // no position at all (pruned, or rooted at a virtual one), which it reports at row
            // 0: never wholly above.
            let bottom = i64::from(info.viewport_row).saturating_add(i64::from(info.grid_rows));
            let wholly_above = !info.viewport_visible && info.grid_rows > 0 && bottom <= 0;
            let listed = info.viewport_visible || (above_due && wholly_above);
            if !listed {
                continue;
            }
            let Some(line) = live_first.checked_add_signed(i64::from(info.viewport_row)) else {
                continue;
            };
            let placement = Placement {
                image: id,
                generation: 0,
                col: info.viewport_col,
                line: LineIndex(line),
                cols: info.grid_cols,
                rows: info.grid_rows,
                x_offset: p.x_offset()?,
                y_offset: p.y_offset()?,
                width: info.pixel_width,
                height: info.pixel_height,
                source: PixelRect {
                    x: info.source_x,
                    y: info.source_y,
                    width: info.source_width,
                    height: info.source_height,
                },
                z: p.z()?,
            };
            if !info.viewport_visible {
                above.push(placement);
                continue;
            }
            let seq = self.seq;
            if let Some(shipped) =
                shipped(&mut self.ledger, &mut self.uploads, seq, placement, &image)?
            {
                out.push(shipped);
            }
        }
        // The last frame's runs whose rows this frame's screen starts below have scrolled up.
        let gone_up = |p: &Placement| p.line.0.saturating_add(u64::from(p.rows)) <= first;
        let mut screen_runs = std::mem::take(&mut self.screen_runs);
        self.runs_above.extend(screen_runs.iter().copied().filter(gone_up));
        screen_runs.clear();
        let base = self.base;
        self.runs_above.retain(|p| p.line.0 >= base);
        let excess = self.runs_above.len().saturating_sub(MAX_ABOVE);
        self.runs_above.drain(..excess);
        let above = if above_due {
            above.extend_from_slice(&self.runs_above);
            // The nearest the screen, which a scroll back reaches first. They are stamped a
            // frame older than the screen's, so over the cache budget they go before those.
            above.sort_by_key(|p| std::cmp::Reverse(p.line));
            above.truncate(MAX_ABOVE);
            above.reverse();
            let stamp = self.seq.saturating_sub(1);
            let mut kept = Vec::with_capacity(above.len());
            for placement in above {
                let Some(image) = graphics.image(placement.image) else { continue };
                if let Some(shipped) =
                    shipped(&mut self.ledger, &mut self.uploads, stamp, placement, &image)?
                {
                    kept.push(shipped);
                }
            }
            Some(kept)
        } else {
            None
        };
        let cell =
            (u32::from(self.size.metrics.cell_width), u32::from(self.size.metrics.cell_height));
        for run in runs {
            // A placement id names one placement; 0 takes the image's first virtual one.
            let Some(v) = virtuals.iter().find(|v| {
                v.image == run.image && (run.placement == 0 || v.placement == run.placement)
            }) else {
                continue;
            };
            let Some(image) = graphics.image(run.image) else { continue };
            let size = (image.width()?, image.height()?);
            let Some(r) = placeholder::render(run, size, v.grid.resolved(size, cell), cell) else {
                continue;
            };
            let placement = Placement {
                image: run.image,
                generation: 0,
                col: i32::from(run.x),
                line: LineIndex(first.saturating_add(u64::from(run.y))),
                cols: run.width,
                rows: 1,
                x_offset: r.x_offset,
                y_offset: r.y_offset,
                width: r.width,
                height: r.height,
                source: r.source,
                z: v.z,
            };
            screen_runs.push(placement);
            let seq = self.seq;
            if let Some(shipped) =
                shipped(&mut self.ledger, &mut self.uploads, seq, placement, &image)?
            {
                out.push(shipped);
            }
        }
        self.screen_runs = screen_runs;
        self.ledger.prune();
        Ok((out, above))
    }

    /// Send the clean rows `rows` again with their wrap now (see `row_above_wraps`). Only the
    /// flag changed, so each sends the line `shown` holds flagged anew, or one read from the
    /// grid when it holds none (never during a hold, whose grid is past the rows shown).
    #[cold]
    #[inline(never)]
    fn rewrap(
        &self,
        rows: &[(u16, bool)],
        shown: &mut [Option<Arc<Line>>],
        updates: &mut Vec<RowUpdate>,
        held: bool,
        scrollback: u64,
    ) -> Result<(), EngineError> {
        for &(at, wraps) in rows {
            let Some(slot) = shown.get_mut(usize::from(at)) else { continue };
            let line = match slot.as_deref() {
                Some(line) => {
                    let mut line = line.clone();
                    line.flags.set(LineFlags::WRAPPED, wraps);
                    line
                }
                None if !held => self.read_line(
                    u32::try_from(scrollback.saturating_add(u64::from(at))).unwrap_or(u32::MAX),
                    self.size.cols,
                )?,
                None => continue,
            };
            let line = Arc::new(line);
            *slot = Some(Arc::clone(&line));
            updates.push(RowUpdate { row: at, line });
        }
        updates.sort_unstable_by_key(|u| u.row);
        Ok(())
    }

    /// Give each cell of screen row `screen_y`'s `line` that holds only a background colour
    /// its colour (see [`paint_backgrounds`]).
    #[cold]
    #[inline(never)]
    fn paint_line_backgrounds(
        &self,
        line: &mut Line,
        screen_y: u32,
        layout: Option<&CellLayout>,
    ) -> Result<(), EngineError> {
        for (x, slot) in (0..).zip(line.cells.iter_mut()) {
            let raw =
                self.term.grid_ref(Point::Screen(PointCoordinate { x, y: screen_y }))?.cell()?;
            let tag = cell_fields(layout, raw)?.content_tag;
            if background_only(tag) {
                slot.style = background_style(tag, raw)?;
            }
        }
        Ok(())
    }

    /// One row of the grid as a [`Line`]. What `FetchLines` serves, 4096 rows at a time: the
    /// row's flags say which per-cell lookups can be skipped (no styling, no multi-codepoint
    /// clusters, no links), a style is resolved once per run of cells sharing it, and a cell's
    /// text never goes through a heap string (MEASUREMENTS.md "history fetch").
    fn read_line(&self, screen_y: u32, cols: u16) -> Result<Line, EngineError> {
        let mut line = Line::blank(cols);
        if cols == 0 {
            return Ok(line);
        }
        let row =
            self.term.grid_ref(Point::Screen(PointCoordinate { x: 0, y: screen_y }))?.row()?;
        let (styled, clusters, row_has_links) =
            (row.is_styled()?, row.has_grapheme_cluster()?, row.has_hyperlink()?);
        let mut prompt_row = row.semantic_prompt().is_ok_and(|p| p != RowSemanticPrompt::None);
        let layout = CellLayout::linked();
        let mut chars = [char::MIN; 16];
        let mut first_semantic = None;
        let mut first_input = None;
        let mut links = LinkRuns::default();
        let mut uri_buf = Vec::new();
        // Style ids name styles within one page, and a row never spans two.
        let mut last_style = None;
        for x in 0..cols {
            let gr = self.term.grid_ref(Point::Screen(PointCoordinate { x, y: screen_y }))?;
            let f = cell_fields(layout, gr.cell()?)?;
            // The input column matters on a prompt's rows only; an output row stops looking
            // after its first cell.
            if first_input.is_none() && (x == 0 || prompt_row) {
                if first_semantic.is_none() {
                    first_semantic = Some(f.semantic_content);
                    prompt_row |= matches!(f.semantic_content, CellSemanticContent::Prompt);
                }
                if matches!(f.semantic_content, CellSemanticContent::Input) {
                    first_input = Some(x);
                }
            }
            let width = convert::cell_width(f.wide);
            let style = if styled && f.has_styling() {
                match last_style {
                    Some((last, style)) if last == f.style_id => style,
                    _ => {
                        let style = convert::style(&gr.style()?);
                        last_style = Some((f.style_id, style));
                        style
                    }
                }
            } else {
                Style::DEFAULT
            };
            if row_has_links {
                if uri_buf.is_empty() {
                    uri_buf.resize(256, 0);
                }
                let uri = if f.hyperlink { hyperlink_uri(&gr, &mut uri_buf)? } else { None };
                links.push(x, uri, width == CellWidth::SpacerTail);
            }
            // Zero for a blank cell and for one holding only a background colour.
            let codepoint = if width.draws_text() { f.codepoint } else { 0 };
            let text = if codepoint == 0 {
                CellText::EMPTY
            } else if clusters && f.content_tag == CellContentTag::CodepointGrapheme {
                match gr.graphemes(&mut chars) {
                    Ok(n) => cluster_text(chars.get(..n).unwrap_or_default()),
                    Err(libghostty_vt::Error::OutOfSpace { required }) => {
                        let mut big = vec![char::MIN; required];
                        let n = gr.graphemes(&mut big)?;
                        cluster_text(big.get(..n).unwrap_or_default())
                    }
                    Err(e) => return Err(e.into()),
                }
            } else {
                char::from_u32(codepoint).map_or(CellText::EMPTY, CellText::from_char)
            };
            set_cell(&mut line, x, Cell { text, style, width });
        }
        line.links = links.finish(cols);
        // After the row, as in a frame: a check in the loop above costs every cell.
        if row.has_background()? {
            self.paint_line_backgrounds(&mut line, screen_y, layout)?;
        }
        let wrapped = match screen_y.checked_sub(1) {
            Some(above) => self
                .term
                .grid_ref(Point::Screen(PointCoordinate { x: 0, y: above }))?
                .row()?
                .is_wrapped()?,
            None => false,
        };
        line.flags.set(LineFlags::WRAPPED, wrapped);
        let abs = self.base.saturating_add(u64::from(screen_y));
        line.mark = first_semantic.map_or(SemanticMark::Unknown, |first| {
            convert::semantic_mark(
                row.semantic_prompt().unwrap_or(RowSemanticPrompt::None),
                first,
                self.prompt_starts.contains(&abs),
                exit_for(&self.exit_marks, &self.prompt_starts, abs),
                first_input,
            )
        });
        Ok(line)
    }
}

/// A fresh terminal's pen: no style, no hyperlink, no protection, and output content
/// (`OSC 133;D`, the one step to output that never takes a row's prompt flag off).
const FRESH_PEN: &[u8] = b"\x1b[0m\x1b]8;;\x1b\\\x1b[0\"q\x1b]133;D\x1b\\";

/// The character sets as a fresh terminal prints with them: ASCII, which libghostty prints as
/// it does its default UTF-8, designated into G0 to G3 (`ESC ( B` and the like), G0 invoked
/// into GL (`SI`) and G2 into GR (`LS2R`).
const FRESH_CHARSETS: &[u8] = b"\x1b(B\x1b)B\x1b*B\x1b+B\x0f\x1b}";

/// The sequences that put every mode `blob` sets back to its default, in order. The alternate
/// screen switches themselves are left alone; the caller enters the alternate screen itself.
fn mode_resets(blob: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for at in memchr::memmem::find_iter(blob, b"\x1b[") {
        let rest = blob.get(at.saturating_add(2)..).unwrap_or_default();
        let (private, rest) = match rest.split_first() {
            Some((b'?', rest)) => (true, rest),
            _ => (false, rest),
        };
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        let Some(&last) = rest.get(digits) else { continue };
        if digits == 0 || !matches!(last, b'h' | b'l') {
            continue;
        }
        let number = rest.get(..digits).unwrap_or_default();
        if private && matches!(number, b"47" | b"1047" | b"1049" | b"1048") {
            continue;
        }
        out.extend_from_slice(b"\x1b[");
        if private {
            out.push(b'?');
        }
        out.extend_from_slice(number);
        out.push(if last == b'h' { b'l' } else { b'h' });
    }
    out
}

/// The tail of `chunk` from its last escape when that tail is an unfinished start of an
/// alternate-screen switch (a proper prefix of `ESC [ ? 1049 h` and friends), else empty.
fn alt_prefix_of(chunk: &[u8]) -> &[u8] {
    let Some(esc) = memchr::memrchr(0x1b, chunk) else { return &[] };
    let tail = chunk.get(esc..).unwrap_or_default();
    let unfinished = [&b"\x1b[?1049h"[..], b"\x1b[?1047h", b"\x1b[?47h"]
        .iter()
        .any(|seq| seq.len() > tail.len() && seq.starts_with(tail));
    if unfinished { tail } else { &[] }
}

/// Byte offset of the first sequence that enters the alternate screen (`CSI ? 1049 h`,
/// `CSI ? 1047 h`, `CSI ? 47 h`), if the chunk holds one. A split sequence (the `ESC [ ?` at the
/// end of one read and the digits in the next) is not found; the primary snapshot is then the
/// one from the last checkpoint, which is only staler by the output between them.
fn alt_enter_at(chunk: &[u8]) -> Option<usize> {
    let mut from = 0;
    while let Some(rel) = memchr::memmem::find(chunk.get(from..)?, b"\x1b[?") {
        let at = from.saturating_add(rel);
        let params = chunk.get(at.saturating_add(3)..).unwrap_or_default();
        if [&b"1049h"[..], b"1047h", b"47h"].iter().any(|p| params.starts_with(p)) {
            return Some(at);
        }
        from = at.saturating_add(3);
    }
    None
}

/// OSC 7 payload (`file://host/percent%20encoded/path`) → local path. Non-file URLs and paths on
/// other hosts are ignored.
#[must_use]
pub fn cwd_from_osc7(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let (host, path) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
    if !(host.is_empty() || host == "localhost" || host.eq_ignore_ascii_case(hostname())) {
        return None;
    }
    if path.is_empty() {
        return None;
    }
    Some(percent_decode(path))
}

/// This machine's name, as a shell's `$HOST` spells it in an OSC 7 URL.
fn hostname() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| rustix::system::uname().nodename().to_string_lossy().into_owned())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let decoded = (b == b'%')
            .then(|| bytes.get(i.saturating_add(1)..i.saturating_add(3)))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        if let Some(v) = decoded {
            out.push(v);
            i = i.saturating_add(3);
        } else {
            out.push(b);
            i = i.saturating_add(1);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The status a prompt starting at absolute `line` should carry: the newest `D` on it or within
/// [`EXIT_LOOKBACK_ROWS`] above it, unless another prompt started in between and took it.
fn exit_for(marks: &BTreeMap<u64, Option<u8>>, starts: &BTreeSet<u64>, line: u64) -> Option<u8> {
    let from = line.saturating_sub(EXIT_LOOKBACK_ROWS);
    let (&at, &exit) = marks.range(from..=line).next_back()?;
    if starts.range(at..line).next().is_some() {
        return None;
    }
    exit
}

/// Whether a prompt starts on absolute `line`, and the status it carries if so.
fn prompt_at(
    marks: &BTreeMap<u64, Option<u8>>,
    starts: &BTreeSet<u64>,
    line: u64,
) -> (bool, Option<u8>) {
    let start = starts.contains(&line);
    (start, if start { exit_for(marks, starts, line) } else { None })
}

/// Change the marks on `line`, adding to `remarked` the rows whose prompt that changed: `line`
/// and the rows below it a status on it reaches.
fn remark(
    marks: &mut BTreeMap<u64, Option<u8>>,
    starts: &mut BTreeSet<u64>,
    remarked: &mut BTreeSet<u64>,
    line: u64,
    change: impl FnOnce(&mut BTreeMap<u64, Option<u8>>, &mut BTreeSet<u64>),
) {
    let reach = line..=line.saturating_add(EXIT_LOOKBACK_ROWS);
    let before: Vec<_> = reach.clone().map(|l| prompt_at(marks, starts, l)).collect();
    change(marks, starts);
    for (l, was) in reach.zip(before) {
        if prompt_at(marks, starts, l) != was {
            remarked.insert(l);
        }
    }
}

/// Collects the OSC 8 runs of one row while its cells are walked left to right.
#[derive(Default)]
struct LinkRuns {
    runs: Vec<Hyperlink>,
    /// The run in progress: first column and URI.
    open: Option<(u16, String)>,
}

impl LinkRuns {
    /// Cell `x` carries `uri` (`None` when it has no link). A spacer tail continues the run of
    /// the wide character it belongs to.
    fn push(&mut self, x: u16, uri: Option<&[u8]>, spacer_tail: bool) {
        match (&self.open, uri) {
            (Some((_, open)), Some(uri)) if open.as_bytes() == uri => {}
            (Some(_), None) if spacer_tail => {}
            _ => {
                self.close(x);
                if let Some(uri) = uri {
                    self.open = Some((x, String::from_utf8_lossy(uri).into_owned()));
                }
            }
        }
    }

    fn close(&mut self, end: u16) {
        if let Some((col, uri)) = self.open.take() {
            let len = end.saturating_sub(col);
            if len > 0 {
                self.runs.push(Hyperlink { col, len, uri });
            }
        }
    }

    fn finish(mut self, cols: u16) -> Vec<Hyperlink> {
        self.close(cols);
        self.runs
    }
}

/// The OSC 8 URI of the cell at `gr`, read into `buf`; `None` when the cell has no link.
fn hyperlink_uri<'b>(
    gr: &GridRef<'_>,
    buf: &'b mut Vec<u8>,
) -> Result<Option<&'b [u8]>, EngineError> {
    let n = match gr.hyperlink_uri(buf) {
        Ok(n) => n,
        Err(libghostty_vt::Error::OutOfSpace { required }) => {
            buf.resize(required, 0);
            gr.hyperlink_uri(buf)?
        }
        Err(e) => return Err(e.into()),
    };
    Ok((n > 0).then(|| buf.get(..n).unwrap_or_default()))
}

/// A grapheme cluster's text, spelled on the stack: a cluster is almost always far shorter
/// than the inline buffer, and only a longer one is collected into a string.
fn cluster_text(chars: &[char]) -> CellText {
    let mut buf = [0_u8; 64];
    let mut len = 0_usize;
    for &c in chars {
        let Some(slot) = buf.get_mut(len..len.saturating_add(c.len_utf8())) else {
            return CellText::from_cluster(&chars.iter().collect::<String>());
        };
        len = len.saturating_add(c.encode_utf8(slot).len());
    }
    std::str::from_utf8(buf.get(..len).unwrap_or_default())
        .map_or(CellText::EMPTY, CellText::from_cluster)
}

/// Whether a cell holds only a background colour (what an erase or a scroll under a coloured
/// pen leaves): it has no style of its own, the colour is its content, and no row flag says
/// the row holds one.
const fn background_only(content: CellContentTag) -> bool {
    matches!(content, CellContentTag::BgColorPalette | CellContentTag::BgColorRgb)
}

/// Give each cell of `line` that holds only a background colour its colour.
fn paint_backgrounds(
    line: &mut Line,
    layout: Option<&CellLayout>,
    cells: impl Iterator<Item = VtCell>,
) -> Result<(), libghostty_vt::Error> {
    for (slot, rc) in line.cells.iter_mut().zip(cells) {
        let tag = cell_fields(layout, rc)?.content_tag;
        if background_only(tag) {
            slot.style = background_style(tag, rc)?;
        }
    }
    Ok(())
}

/// The style of a cell [`background_only`] holds.
#[cold]
fn background_style(content: CellContentTag, cell: VtCell) -> Result<Style, libghostty_vt::Error> {
    let bg = match content {
        CellContentTag::BgColorRgb => {
            let RgbColor { r, g, b } = cell.bg_color_rgb()?;
            slopty_grid::Color::Rgb(r, g, b)
        }
        _ => slopty_grid::Color::Palette(cell.bg_color_palette()?.0),
    };
    Ok(Style { bg, ..Style::DEFAULT })
}

/// Every field of a cell, decoded with the linked build's layout (looked up once per frame
/// or row by the caller) and read through libghostty's getters when there is none.
fn cell_fields(
    layout: Option<&CellLayout>,
    cell: VtCell,
) -> Result<CellFields, libghostty_vt::Error> {
    match layout {
        Some(layout) => layout.decode(cell),
        None => cell.fields(),
    }
}

fn set_cell(line: &mut Line, x: u16, cell: Cell) {
    if let Some(slot) = line.cells.get_mut(usize::from(x)) {
        *slot = cell;
    }
}

/// A frame for viewers joining, with what goes around it (see [`GhosttyEngine::join_frame`]
/// and [`GhosttyEngine::baseline_frame`]).
#[derive(Debug)]
pub struct Joined {
    /// Every row.
    pub frame: Frame,
    /// The images it places that the joiners need, sent ahead of it.
    pub images: Vec<ImageUpload>,
}

/// What a frame places: on its screen, and above it when that is due ([`Frame::above`]).
type Placed = (Vec<Placement>, Option<Vec<Placement>>);

/// A virtual kitty placement: shown only through placeholder cells.
struct Virtual {
    image: u32,
    placement: u32,
    grid: placeholder::Grid,
    z: i32,
}

/// Queue `image`'s pixels unless the clients hold this generation already; the generation and
/// the shrink factor they hold it at, or `None` while the transmission is incomplete.
fn ship(
    ledger: &mut Ledger,
    uploads: &mut Vec<ImageUpload>,
    seq: u64,
    id: u32,
    image: &kitty_graphics::Image<'_>,
) -> Result<Option<(u64, u32)>, EngineError> {
    let generation = image.generation()?;
    let shrink = if let Some(shrink) = ledger.held(id, generation) {
        shrink
    } else {
        let Some(data) = image.data()? else { return Ok(None) };
        let (w, h) = (image.width()?, image.height()?);
        let Some((up, shrink)) = graphics::upload(id, generation, image.format()?, w, h, data)
        else {
            return Ok(None);
        };
        let bytes = up.rgba.len();
        uploads.push(up);
        ledger.insert(id, Shipped { generation, shrink, bytes, last: seq });
        shrink
    };
    ledger.touch(id, seq);
    Ok(Some((generation, shrink)))
}

/// `placement` with the generation the clients hold of `image` and its source rectangle in the
/// pixels they hold, its pixels queued first unless they hold them; stamped `seq` in the
/// ledger. `None` while the transmission is incomplete.
fn shipped(
    ledger: &mut Ledger,
    uploads: &mut Vec<ImageUpload>,
    seq: u64,
    placement: Placement,
    image: &kitty_graphics::Image<'_>,
) -> Result<Option<Placement>, EngineError> {
    let Some((generation, shrink)) = ship(ledger, uploads, seq, placement.image, image)? else {
        return Ok(None);
    };
    let scaled = |v: u32| v.checked_div(shrink).unwrap_or(v);
    let s = placement.source;
    let source = PixelRect {
        x: scaled(s.x),
        y: scaled(s.y),
        width: scaled(s.width),
        height: scaled(s.height),
    };
    Ok(Some(Placement { generation, source, ..placement }))
}

/// A placeholder cell decoded from its wire style and its grapheme cluster (U+10EEEE first,
/// then the diacritics).
/// A blank line of `cols` cells, in `spare`'s allocation when it has one of that width.
fn blank_line(spare: Option<Line>, cols: u16) -> Line {
    match spare {
        Some(mut line) if line.cols() == cols => {
            line.cells.fill(Cell::BLANK);
            line.flags = LineFlags::empty();
            line.mark = SemanticMark::Unknown;
            line.links.clear();
            line
        }
        _ => Line::blank(cols),
    }
}

/// Whether the row above viewport row `y` soft-wraps into it, the row above the viewport
/// for the first. A line continues the one above when that one wrapped: reflow and
/// selection go by that row's flag, and the continuation flag libghostty keeps on the row
/// below can outlive it (a scroll region moved the row above, the history let it go).
fn row_above_wraps(term: &Terminal<'_, '_>, y: u16) -> Result<bool, EngineError> {
    let point = match y.checked_sub(1) {
        Some(above) => Point::Viewport(PointCoordinate { x: 0, y: u32::from(above) }),
        None => match term.scrollback_rows()?.checked_sub(1) {
            Some(above) => {
                Point::Screen(PointCoordinate { x: 0, y: u32::try_from(above).unwrap_or(u32::MAX) })
            }
            None => return Ok(false),
        },
    };
    Ok(term.grid_ref(point)?.row()?.is_wrapped()?)
}

/// The next row a frame reads and its screen row (`y` is the one after the last read): every
/// row, or with `dirty_only` the next one libghostty marks dirty, skipping the clean ones
/// without a call into libghostty each. Kept out of the frame's row loop, whose every cell
/// pays for its size.
#[inline(never)]
fn next_row<'r, 'a, 's>(
    rows: &'r mut RowIteration<'a, 's>,
    dirty_only: bool,
    y: u16,
) -> Option<(u16, &'r RowIteration<'a, 's>)> {
    if dirty_only { rows.next_dirty() } else { rows.next().map(|row| (y, row)) }
}

fn placeholder_cell(style: &Style, cluster: &str) -> placeholder::Cell {
    let id = |c: slopty_grid::Color| match c {
        slopty_grid::Color::Palette(i) => placeholder::color_id(Some(i), None),
        slopty_grid::Color::Rgb(r, g, b) => placeholder::color_id(None, Some((r, g, b))),
        slopty_grid::Color::Default => 0,
    };
    let (fg, underline) = (id(style.fg), id(style.underline_color));
    placeholder::Cell::decode(fg, underline, cluster.chars().skip(1).map(u32::from))
}

const fn check_size(size: TermSize) -> Result<(), EngineError> {
    if size.cols == 0 || size.rows == 0 {
        return Err(EngineError::InvalidSize("zero columns or rows"));
    }
    // The wire refuses a line or a frame past these, so a terminal past them could not be shown.
    if size.cols > slopty_grid::MAX_COLS || size.rows > slopty_grid::MAX_ROWS {
        return Err(EngineError::InvalidSize("past MAX_COLS × MAX_ROWS"));
    }
    if size.metrics.cell_width == 0 || size.metrics.cell_height == 0 {
        return Err(EngineError::InvalidSize("zero cell metrics"));
    }
    Ok(())
}

/// What the engine last told its session of the title and the directory. A full reset clears
/// both in libghostty without a callback, and the session still holds them.
#[derive(Debug, Default)]
struct Reported {
    /// The last title reported was not empty.
    titled: bool,
    /// The `OSC 7` value the last directory reported was read from.
    pwd: String,
}

fn install_callbacks(
    term: &mut Terminal<'static, 'static>,
    events: &Events,
    light: &Rc<std::cell::Cell<bool>>,
    reported: &Rc<RefCell<Reported>>,
) -> Result<(), EngineError> {
    let for_scheme = Rc::clone(light);
    term.on_color_scheme(move |_| Some(scheme(for_scheme.get())))?;
    let for_pty = Rc::clone(events);
    term.on_pty_write(move |_, data: &[u8]| {
        for_pty.borrow_mut().push(EngineEvent::PtyWrite(data.to_vec()));
    })?;
    let for_bell = Rc::clone(events);
    term.on_bell(move |_| for_bell.borrow_mut().push(EngineEvent::Bell))?;
    let for_notify = Rc::clone(events);
    term.on_desktop_notification(move |_, n| {
        // The program's bytes, which libghostty passes on unchecked.
        let title = String::from_utf8_lossy(n.title()).chars().take(NOTIFICATION_CHARS).collect();
        let body = String::from_utf8_lossy(n.body()).chars().take(NOTIFICATION_CHARS).collect();
        for_notify.borrow_mut().push(EngineEvent::Notification { title, body });
    })?;
    let (for_title, title_reported) = (Rc::clone(events), Rc::clone(reported));
    term.on_title_changed(move |t| {
        let title = t.title().unwrap_or_default().to_owned();
        title_reported.borrow_mut().titled = !title.is_empty();
        for_title.borrow_mut().push(EngineEvent::Title(title));
    })?;
    let (for_pwd, pwd_reported) = (Rc::clone(events), Rc::clone(reported));
    term.on_pwd_changed(move |t| {
        let pwd = t.pwd().unwrap_or_default();
        if let Some(path) = cwd_from_osc7(pwd) {
            pwd.clone_into(&mut pwd_reported.borrow_mut().pwd);
            for_pwd.borrow_mut().push(EngineEvent::Cwd(path));
        }
    })?;
    let for_clip = Rc::clone(events);
    term.on_clipboard_write(move |_, write| {
        // Only the system clipboard; selection/primary are X11 notions with no counterpart
        // on the clients. Reads (OSC 52 `?`, OSC 5522) are answered by [`clipboard`].
        let text = (write.location() == ClipboardLocation::Standard)
            .then(|| write.contents().find(|c| c.mime.starts_with("text/plain")))
            .flatten()
            .map(|c| String::from_utf8_lossy(c.data).into_owned());
        let result = match text {
            Some(text) => {
                for_clip.borrow_mut().push(EngineEvent::ClipboardWrite { text });
                Ok(())
            }
            None => Err(libghostty_vt::terminal::ClipboardWriteError::Unsupported),
        };
        write.reply(result, false);
    })?;
    Ok(())
}

/// Queue the prompt marks and full resets libghostty reports during a write, each with the
/// cursor where the shell wrote it: the terminal has applied the bytes before the sequence and
/// none after it. The cursor's row is tracked, not counted, because the rest of the write may
/// scroll it or evict history above it before the engine settles.
fn install_marks(
    term: &mut Terminal<'static, 'static>,
    marks: &Rc<RefCell<Vec<Pending>>>,
) -> Result<(), EngineError> {
    let for_marks = Rc::clone(marks);
    term.on_semantic_prompt(move |t, event| {
        let Some(mark) = osc133::Mark::of(event) else { return };
        let (Ok(y), Ok(col), Ok(screen)) = (t.cursor_y(), t.cursor_x(), t.active_screen()) else {
            return;
        };
        let row = t.track_grid_ref(Point::Active(PointCoordinate { x: 0, y: u32::from(y) })).ok();
        for_marks.borrow_mut().push(Pending::Mark { mark, row, col, screen });
    })?;
    let for_reset = Rc::clone(marks);
    term.on_reset(move |_| for_reset.borrow_mut().push(Pending::Reset))?;
    Ok(())
}

/// Follow the program's `OSC 9;4` progress reports, and report each change.
fn install_progress(
    term: &mut Terminal<'static, 'static>,
    events: &Events,
    progress: &Rc<std::cell::Cell<Progress>>,
) -> Result<(), EngineError> {
    let (events, shown) = (Rc::clone(events), Rc::clone(progress));
    term.on_progress_report(move |_, report| {
        let Ok(state) = report.state() else { return };
        let was = shown.get();
        let now = progress_after(was, state, report.progress());
        if now != was {
            shown.set(now);
            events.borrow_mut().push(EngineEvent::Progress(now));
        }
    })?;
    Ok(())
}

/// The progress after a report of `state` with `percent`, given the progress before it. An
/// error or a pause without a value keeps the last value, and a set without one is zero, as in
/// `ConEmu`, which defined the sequence.
fn progress_after(was: Progress, state: VtProgress, percent: Option<u8>) -> Progress {
    let percent = percent.map(|p| p.min(100));
    let (state, percent) = match state {
        VtProgress::Set => (ProgressState::Set, Some(percent.unwrap_or(0))),
        VtProgress::Error => (ProgressState::Error, percent.or(was.percent)),
        VtProgress::Indeterminate => (ProgressState::Indeterminate, None),
        VtProgress::Pause => (ProgressState::Paused, percent.or(was.percent)),
        // `Remove`, and any state a later libghostty adds.
        _ => (ProgressState::None, None),
    };
    Progress { state, percent }
}

/// Capture the frame a program leaves on screen when it begins a render hold (synchronized
/// output), into the render state frames are built from until the hold ends.
fn install_render_hold(
    term: &mut Terminal<'static, 'static>,
    render: &Rc<RefCell<RenderState<'static>>>,
    hold: &Rc<std::cell::Cell<Option<Hold>>>,
) -> Result<(), EngineError> {
    let (render, hold) = (Rc::clone(render), Rc::clone(hold));
    term.on_render_hold(move |t, held| {
        if !held {
            hold.set(None);
            return;
        }
        // Nothing after the start of the hold is processed yet: this is the finished frame.
        // Without the capture the hold is not honoured, and frames show the live screen.
        let captured = render.try_borrow_mut().is_ok_and(|mut r| r.update(t).is_ok());
        let scrollback = t.scrollback_rows().map(|n| n as u64);
        hold.set(match (captured, scrollback) {
            (true, Ok(scrollback)) => Some(Hold { since: MonoTime::now(), scrollback }),
            _ => None,
        });
    })?;
    Ok(())
}

/// Whether a background reads as light, as the theme judges its own colours.
const fn is_light([r, g, b]: [u8; 3]) -> bool {
    slopty_theme::Rgb { r, g, b }.is_light()
}

/// The scheme a light or dark background is reported as.
const fn scheme(light: bool) -> ColorScheme {
    if light { ColorScheme::Light } else { ColorScheme::Dark }
}

/// Make `colors` libghostty's defaults: what OSC 10/11/12 `?` and OSC 4 queries answer.
/// Without defaults libghostty answers the first three with nothing, and a TUI that asks
/// (neovim, helix, delta) waits its timeout or guesses.
fn set_colors(term: &mut Terminal<'_, '_>, colors: &TermColors) -> Result<(), EngineError> {
    let rgb = |[r, g, b]: [u8; 3]| RgbColor { r, g, b };
    term.set_default_fg_color(Some(rgb(colors.fg)))?
        .set_default_bg_color(Some(rgb(colors.bg)))?
        .set_default_cursor_color(Some(rgb(colors.cursor)))?;
    let mut palette = term.default_color_palette()?;
    for (index, &color) in (0_u8..).zip(colors.ansi.iter()) {
        palette.set(PaletteIndex(index), rgb(color));
    }
    term.set_default_color_palette(Some(palette))?;
    Ok(())
}

/// The program's colour changes as the sequences that made them (OSC 10/11/12, OSC 4), so a
/// checkpoint replays them and nothing else about the colours.
fn colour_sets(set: &ColorOverrides) -> Vec<u8> {
    let mut out = Vec::new();
    let rgb = |[r, g, b]: [u8; 3]| format!("rgb:{r:02x}/{g:02x}/{b:02x}");
    for (code, color) in [(10, set.fg), (11, set.bg), (12, set.cursor)] {
        if let Some(color) = color {
            out.extend_from_slice(format!("\x1b]{code};{}\x1b\\", rgb(color)).as_bytes());
        }
    }
    for &(index, color) in &set.palette {
        out.extend_from_slice(format!("\x1b]4;{index};{}\x1b\\", rgb(color)).as_bytes());
    }
    out
}

/// What the program changed over the defaults (OSC 4/10/11/12): each current colour that
/// differs from its default.
fn overrides(term: &Terminal<'_, '_>) -> Result<ColorOverrides, EngineError> {
    let bytes = |c: RgbColor| [c.r, c.g, c.b];
    let changed = |now: Option<RgbColor>, default: Option<RgbColor>| {
        now.filter(|&n| Some(n) != default).map(bytes)
    };
    let fg = changed(term.fg_color()?, term.default_fg_color()?);
    let bg = changed(term.bg_color()?, term.default_bg_color()?);
    let cursor = changed(term.cursor_color()?, term.default_cursor_color()?);
    let (now, default) = (term.color_palette()?, term.default_color_palette()?);
    let palette = (0..=u8::MAX)
        .zip(now.0.iter().zip(default.0.iter()))
        .filter(|(_, (n, d))| n != d)
        .map(|(i, (n, _))| (i, bytes(*n)))
        .collect();
    Ok(ColorOverrides { fg, bg, cursor, palette })
}

/// Whether `bytes` may change the colours or the pointer shape: OSC sequences set them (the
/// colours 4, 10-12, 104, 110-112 and 21, the pointer 22) and RIS (`ESC c`) resets the colours.
fn may_touch_osc_state(bytes: &[u8]) -> bool {
    memchr::memchr_iter(0x1b, bytes)
        .any(|at| matches!(bytes.get(at.saturating_add(1)), Some(b']' | b'c')))
}

impl GhosttyEngine {
    /// Feed PTY output.
    pub fn write(&mut self, bytes: &[u8]) {
        if std::mem::take(&mut self.fresh) && carried::holds(bytes) {
            self.restore(bytes);
        } else {
            self.feed(bytes);
        }
        self.after_dnd();
        // libghostty has no colour-change or pointer-change callback. The current colours
        // against the defaults are eight reads and two palette copies, so they and the pointer
        // are looked at only after a write that could have changed them, and once more after
        // that for a sequence split across two writes.
        let touched = may_touch_osc_state(bytes);
        if !touched && !std::mem::replace(&mut self.colours_touched, false) {
            return;
        }
        self.colours_touched = touched;
        if let Ok(shape) = self.term.mouse_shape().map(convert::pointer)
            && shape != self.pointer
        {
            self.pointer = shape;
            self.events.borrow_mut().push(EngineEvent::Pointer(shape));
        }
        if let Ok(now) = overrides(&self.term)
            && now != self.overrides
        {
            self.overrides = now.clone();
            self.generation = self.generation.wrapping_add(1);
            self.events.borrow_mut().push(EngineEvent::Colors(now));
        }
    }

    /// Resize; reflows the primary screen and invalidates line numbering.
    ///
    /// # Errors
    ///
    /// A zero size or cell metric, or libghostty-vt failing.
    pub fn resize(&mut self, size: TermSize) -> Result<(), EngineError> {
        check_size(size)?;
        if size == self.size {
            return Ok(());
        }
        let reflow = size.cols != self.size.cols || size.rows != self.size.rows;
        if reflow {
            // A tracked pin keeps its row from ghostty's shrink trim of blank rows
            // (`PageList.trimTrailingBlankRows`): pinned to the bottom row, the anchor would push
            // the screen into history instead, rows above the cursor and all. The reflow starts a
            // new numbering, so the anchors have nothing left to hold.
            self.anchor = None;
            self.primary_anchor = None;
        }
        if size.cols != self.size.cols {
            self.clear_prompt_before_reflow()?;
        }
        self.term.resize(
            size.cols,
            size.rows,
            u32::from(size.metrics.cell_width),
            u32::from(size.metrics.cell_height),
        )?;
        self.size = size;
        self.generation = self.generation.wrapping_add(1);
        if reflow {
            self.primary_commands.clear();
            self.primary_marks = None;
            self.bump_epoch();
            self.reanchor()?;
        }
        Ok(())
    }

    /// Current size.
    #[must_use]
    pub const fn size(&self) -> TermSize {
        self.size
    }

    /// The next diff for every viewer, if anything changed since the last frame. `input_ack`
    /// is the highest key sequence number whose bytes reached the PTY before this frame's
    /// output was consumed. During a render hold it is the frame the program left on screen
    /// when the hold began, once.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn take_frame(&mut self, input_ack: u64) -> Result<Option<Frame>, EngineError> {
        self.build_frame(input_ack, Take::Diff)
    }

    /// Every row, for every viewer (a resize): the next sequence number, and every image
    /// shipped again. The rows sent are what the next diff is taken against.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn full_frame(&mut self, input_ack: u64) -> Result<Frame, EngineError> {
        self.build_frame(input_ack, Take::Everyone)?.ok_or(EngineError::InvalidSize("empty frame"))
    }

    /// Every row for one viewer joining (attach, catching up) beside viewers that follow the
    /// diffs, with the images it needs sent ahead. The frame carries the sequence number of the
    /// last frame the others had, and what changed since is still theirs to take: take the
    /// pending diff first ([`Self::take_frame`]), so it does not reach the joiner as a gap.
    /// With nobody following the diffs, [`Self::baseline_frame`].
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn join_frame(&mut self, input_ack: u64) -> Result<Joined, EngineError> {
        let others = std::mem::take(&mut self.uploads);
        let frame = self.build_frame(input_ack, Take::Joiner);
        let images = std::mem::replace(&mut self.uploads, others);
        let frame = frame?.ok_or(EngineError::InvalidSize("empty frame"))?;
        Ok(Joined { frame, images })
    }

    /// Every row for viewers joining while nobody follows the diffs (the first attach, a
    /// re-attach, catching up alone), with every image it places sent ahead, at the next
    /// sequence number. What they are sent becomes what the next diff is taken against, so the
    /// first key after it carries the rows it changed, not every row again as a diff after
    /// [`Self::join_frame`] would.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn baseline_frame(&mut self, input_ack: u64) -> Result<Joined, EngineError> {
        // Owed to viewers that no longer follow, and this frame ships every image it places.
        self.uploads.clear();
        let frame = self.full_frame(input_ack)?;
        Ok(Joined { frame, images: std::mem::take(&mut self.uploads) })
    }

    /// Nobody is watching: drop what the next frame would have carried that the next joiner's
    /// frame does not, without building it.
    pub fn discard_frame(&mut self) {
        self.forced_rows.clear();
        self.remarked_rows.clear();
        self.uploads.clear();
        self.block_news.clear();
    }

    /// Scrollback lines by absolute index. Lines outside `[oldest, total)` are omitted, so the
    /// result may be shorter than `count`; it starts at `start` clamped to `oldest`.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn lines(
        &self,
        start: LineIndex,
        count: u32,
    ) -> Result<(LineIndex, Vec<Line>), EngineError> {
        let total = self.total_lines()?;
        let first = start.0.max(self.base);
        // The range asked for, less what was dropped: a range wholly below the oldest line
        // kept is empty, not the lines after it.
        let end = start.0.saturating_add(u64::from(count)).min(total).max(first);
        if first < total.saturating_sub(u64::from(self.size.rows)) {
            // Reading compressed history restores its pages: the next idle pass takes them.
            self.compressed_at.set(None);
        }
        let mut out = Vec::with_capacity(usize::try_from(end.saturating_sub(first)).unwrap_or(0));
        let cols = self.size.cols;
        let mut abs = first;
        while abs < end {
            let screen_y = u32::try_from(abs.saturating_sub(self.base)).unwrap_or(u32::MAX);
            out.push(self.read_line(screen_y, cols)?);
            abs = abs.saturating_add(1);
        }
        Ok((LineIndex(first), out))
    }

    /// Find `needle` in the retained history and the screen (see [`search::find`]); `regex`
    /// treats it as a pattern.
    ///
    /// # Errors
    ///
    /// [`EngineError::Pattern`] when the pattern does not compile; libghostty-vt failing.
    pub fn search(
        &mut self,
        needle: &str,
        regex: bool,
        max: u32,
    ) -> Result<search::Found, EngineError> {
        if needle.is_empty() {
            return Ok(search::Found::default());
        }
        let pattern = search::Pattern::new(needle, regex).map_err(EngineError::Pattern)?;
        // A history row is formatted once, by the first search after it scrolled up; the
        // screen, which a program may still write anywhere, every time.
        let scrollback = self.term.scrollback_rows()? as u64;
        let y = |rows: u64| u32::try_from(rows).unwrap_or(u32::MAX);
        let history_end = self.base.saturating_add(scrollback);
        if let Some((from, to)) =
            self.history.missing(self.epoch, self.size.cols, self.base, history_end)
        {
            let (first, last) = (from.saturating_sub(self.base), to.saturating_sub(self.base));
            let (top, bottom) = (y(first), y(last.saturating_sub(1)));
            let text = self.plain_rows(top, bottom)?;
            let wraps = self.row_wraps(top, bottom)?;
            self.history.append(&text, to.saturating_sub(from), &wraps);
        }
        let rows = self.total_rows()?;
        let (screen, wraps) = if rows > scrollback {
            let (top, bottom) = (y(scrollback), y(rows.saturating_sub(1)));
            (self.plain_rows(top, bottom)?, self.row_wraps(top, bottom)?)
        } else {
            (String::new(), Vec::new())
        };
        let screen = screen
            .split('\n')
            .enumerate()
            .map(|(i, t)| (t, wraps.get(i).copied().unwrap_or(false)));
        Ok(self.history.find(&pattern, needle, regex, max, screen))
    }

    /// Encode a key event into `out`. Appends nothing for keys the terminal does not encode.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn encode_key(&mut self, event: &KeyEvent, out: &mut Vec<u8>) -> Result<(), EngineError> {
        let ev = &mut self.key_ev;
        ev.set_action(convert::key_action(event.action))
            .set_key(convert::key(event.code))
            .set_mods(convert::mods(event.mods))
            .set_consumed_mods(convert::mods(event.consumed_mods))
            .set_composing(event.composing)
            .set_utf8(event.text.as_deref());
        if let Some(c) = event.unshifted {
            ev.set_unshifted_codepoint(c);
        }
        // The terminal's modes reset option-as-alt; the client's choice goes on after them.
        let option_as_alt =
            if event.option_as_alt { key::OptionAsAlt::True } else { key::OptionAsAlt::False };
        self.key_enc.set_options_from_terminal(&self.term).set_macos_option_as_alt(option_as_alt);
        match self.key_enc.encode_to_vec(ev, out) {
            // Not every key produces bytes; the encoder reports that as an invalid value.
            Ok(()) | Err(libghostty_vt::Error::InvalidValue) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Encode a mouse event into `out` according to the active tracking mode and format.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn encode_mouse(
        &mut self,
        event: &MouseEvent,
        out: &mut Vec<u8>,
    ) -> Result<(), EngineError> {
        if let MouseAction::Wheel { rows, .. } = event.action
            && !self.term.is_mouse_tracking()?
        {
            // Alternate scroll (mode 1007, on by default): on the alternate screen a program
            // that never asked for the mouse still gets the wheel as cursor keys, so `less`
            // and a `vim` without mouse mode scroll. The primary screen is scrolled by the
            // client's own cache, so there the wheel means nothing to the program.
            if self.on_alt && self.term.mode(Mode::ALT_SCROLL)? {
                let code = if rows > 0 { KeyCode::ArrowUp } else { KeyCode::ArrowDown };
                let arrow = KeyEvent {
                    seq: 0,
                    action: KeyAction::Press,
                    code,
                    mods: Mods::empty(),
                    consumed_mods: Mods::empty(),
                    text: None,
                    unshifted: None,
                    composing: false,
                    option_as_alt: false,
                };
                for _ in 0..rows.unsigned_abs() {
                    self.encode_key(&arrow, out)?;
                }
            }
            return Ok(());
        }
        let size = self.size;
        self.mouse_enc.set_options_from_terminal(&self.term);
        self.mouse_enc.set_size(mouse::EncoderSize {
            screen_width: size.width_px(),
            screen_height: size.height_px(),
            cell_width: u32::from(size.metrics.cell_width),
            cell_height: u32::from(size.metrics.cell_height),
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        });
        let ev = &mut self.mouse_ev;
        ev.set_mods(convert::mods(event.mods));
        // Pixel positions are bounded by the terminal size (< 2^24), so f32 is exact.
        #[expect(clippy::cast_precision_loss, reason = "bounded by terminal pixel size")]
        ev.set_position(mouse::Position { x: event.px as f32, y: event.py as f32 });

        let emit = |enc: &mut mouse::Encoder<'static>,
                    vt_event: &mouse::Event<'static>,
                    sink: &mut Vec<u8>| {
            match enc.encode_to_vec(vt_event, sink) {
                Ok(()) | Err(libghostty_vt::Error::InvalidValue) => Ok(()),
                Err(e) => Err(EngineError::from(e)),
            }
        };

        match event.action {
            MouseAction::Press | MouseAction::Release => {
                let press = event.action == MouseAction::Press;
                // A second press of a held button, or the release of one never pressed, leaves
                // the set as it was: a count would drift and report a drag after the release.
                let bit = event.button.map_or(0, MouseButton::bit);
                if press {
                    self.buttons_down |= bit;
                } else {
                    self.buttons_down &= !bit;
                }
                ev.set_action(if press { mouse::Action::Press } else { mouse::Action::Release });
                ev.set_button(event.button.map(convert::mouse_button));
                self.mouse_enc.set_any_button_pressed(self.buttons_down > 0);
                emit(&mut self.mouse_enc, ev, out)
            }
            MouseAction::Motion => {
                ev.set_action(mouse::Action::Motion);
                ev.set_button(event.button.map(convert::mouse_button));
                self.mouse_enc.set_any_button_pressed(self.buttons_down > 0);
                emit(&mut self.mouse_enc, ev, out)
            }
            MouseAction::Wheel { rows, cols } => {
                // One press per notch: up = button 4, down = 5, left = 6, right = 7.
                let mut steps: Vec<mouse::Button> = Vec::new();
                let vertical = if rows > 0 { mouse::Button::Four } else { mouse::Button::Five };
                steps.extend(std::iter::repeat_n(vertical, usize::from(rows.unsigned_abs())));
                let horizontal = if cols > 0 { mouse::Button::Six } else { mouse::Button::Seven };
                steps.extend(std::iter::repeat_n(horizontal, usize::from(cols.unsigned_abs())));
                ev.set_action(mouse::Action::Press);
                for button in steps {
                    ev.set_button(Some(button));
                    emit(&mut self.mouse_enc, ev, out)?;
                }
                Ok(())
            }
        }
    }

    /// Whether libghostty's parser stands between sequences, with no escape sequence, control
    /// string or UTF-8 character left open for the next write to finish: where a checkpoint
    /// can cut the output without the rest printing as text after a restart.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn at_ground(&self) -> Result<bool, EngineError> {
        Ok(self.term.is_vt_ground()?)
    }

    /// The terminfo entry the session's programs were given (`TERM`), which an XTGETTCAP
    /// query for `TN` is answered with; without it the query gets no answer.
    ///
    /// # Errors
    ///
    /// A name longer than 128 bytes.
    pub fn set_terminfo_name(&mut self, name: &str) -> Result<(), EngineError> {
        self.term.set_terminfo_name(name)?;
        Ok(())
    }

    /// The pty's line discipline changed (the worker reads `termios` after each read): with
    /// echo off, as at a password prompt, the frames say [`TermModes::ECHO_OFF`] and the
    /// client guesses nothing. Takes effect on the next frame.
    pub fn set_line_discipline(&mut self, discipline: LineDiscipline) {
        if self.line_discipline.replace(discipline) != Some(discipline) {
            self.discipline_changed = true;
        }
    }

    /// Whether `text` pasted now could not run anything by itself (ghostty's
    /// `clipboard-paste-protection`): outside bracketed paste a line break runs what precedes
    /// it, and inside it the bracket's end closes the paste and the rest is typed.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn paste_is_safe(&self, text: &str) -> Result<bool, EngineError> {
        Ok(if self.term.mode(Mode::BRACKETED_PASTE)? {
            !text.contains("\x1b[201~")
        } else {
            !text.contains(['\n', '\r'])
        })
    }

    /// Encode pasted text, bracketed when the program asked for it. Unsafe control characters
    /// are stripped when not bracketed.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn encode_paste(&self, text: &str, out: &mut Vec<u8>) -> Result<(), EngineError> {
        let bracketed = self.term.mode(Mode::BRACKETED_PASTE)?;
        let mut data = text.as_bytes().to_vec();
        let mut buf = vec![0_u8; data.len().saturating_add(16)];
        let written = loop {
            match paste::encode(&mut data, bracketed, &mut buf) {
                Ok(n) => break n,
                Err(libghostty_vt::Error::OutOfSpace { required }) => {
                    buf.resize(required.max(buf.len().saturating_mul(2)), 0);
                }
                Err(e) => return Err(e.into()),
            }
        };
        out.extend_from_slice(buf.get(..written).unwrap_or_default());
        Ok(())
    }

    /// Encode a focus change, if the program asked to be told.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn encode_focus(&self, focused: bool, out: &mut Vec<u8>) -> Result<(), EngineError> {
        if !self.term.mode(Mode::FOCUS_EVENT)? {
            return Ok(());
        }
        let ev = if focused { focus::Event::Gained } else { focus::Event::Lost };
        let mut buf = [0_u8; 8];
        let n = ev.encode(&mut buf)?;
        out.extend_from_slice(buf.get(..n).unwrap_or_default());
        Ok(())
    }

    /// Side effects since the last drain.
    pub fn drain_events(&self) -> Vec<EngineEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    /// Images the frames taken since the last drain place and the viewers do not hold yet:
    /// sent ahead of those frames (see [`graphics::Ledger`]).
    pub fn drain_images(&mut self) -> Vec<ImageUpload> {
        std::mem::take(&mut self.uploads)
    }

    /// The colours the driver paints with: what colour queries (OSC 10/11/12 `?`, OSC 4)
    /// answer from now on. The default is the dark theme's.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn set_colors(&mut self, colors: &TermColors) -> Result<(), EngineError> {
        set_colors(&mut self.term, colors)?;
        let light = is_light(colors.bg);
        // A program that asked to be told (mode 2031) hears a scheme change unprompted.
        if self.light.replace(light) != light && self.term.mode(Mode::COLOR_SCHEME_REPORT)? {
            let mut buf = [0_u8; 16];
            let n = scheme(light).encode_report(&mut buf)?;
            let report = buf.get(..n).unwrap_or_default().to_vec();
            self.events.borrow_mut().push(EngineEvent::PtyWrite(report));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use slopty_grid::{CellWidth, CursorShape, StyleFlags};
    use slopty_proto::input::{CellMetrics, KeyAction, KeyCode, Mods};

    use super::*;

    fn engine(cols: u16, rows: u16) -> GhosttyEngine {
        GhosttyEngine::new(EngineConfig {
            size: TermSize { cols, rows, metrics: CellMetrics { cell_width: 8, cell_height: 16 } },
            scrollback_lines: 100,
        })
        .unwrap()
    }

    #[test]
    fn first_frame_is_full_and_text_lands_in_cells() {
        let mut e = engine(10, 3);
        e.write(b"hi \x1b[1mbold\x1b[0m");
        let f = e.full_frame(0).unwrap();
        assert!(f.full);
        assert_eq!(f.updates.len(), 3);
        let row0 = &f.updates[0].line;
        assert_eq!(row0.text(), "hi bold");
        assert!(row0.cells[3].style.flags.contains(StyleFlags::BOLD));
        assert!(!row0.cells[0].style.flags.contains(StyleFlags::BOLD));
        assert_eq!(f.cursor.col, 7);
        assert_eq!(f.total_lines, 3);
        assert_eq!(f.first_visible_line, LineIndex(0));
    }

    /// Underline styles and colours written with colon subparameters (SGR `4:n`, `58:2::r:g:b`,
    /// `58:5:n`) and their semicolon forms reach the cells the clients paint.
    #[test]
    fn colon_underline_styles_and_colours_reach_the_cells() {
        use slopty_grid::{Color, Underline};
        let mut e = engine(10, 1);
        e.write(b"\x1b[4:3;58:2::255:0:0mA\x1b[4:4;58:5:4mB\x1b[4:5;58;2;1;2;3mC");
        e.write(b"\x1b[59;21mD\x1b[4:0mE\x1b[4:1mF");
        let f = e.full_frame(0).unwrap();
        let cells = &f.updates[0].line.cells;
        let styles: Vec<(Underline, Color)> =
            cells.iter().take(6).map(|c| (c.style.underline, c.style.underline_color)).collect();
        assert_eq!(
            styles,
            [
                (Underline::Curly, Color::Rgb(255, 0, 0)),
                (Underline::Dotted, Color::Palette(4)),
                (Underline::Dashed, Color::Rgb(1, 2, 3)),
                (Underline::Double, Color::Default),
                (Underline::None, Color::Default),
                (Underline::Single, Color::Default),
            ]
        );
    }

    #[test]
    fn partial_frames_carry_only_dirty_rows() {
        let mut e = engine(10, 3);
        e.write(b"a\r\nb\r\nc");
        let _first = e.full_frame(0).unwrap();
        assert!(e.take_frame(0).unwrap().is_none(), "nothing changed");
        e.write(b"\x1b[2;1HB");
        let f = e.take_frame(5).unwrap().unwrap();
        assert!(!f.full);
        assert_eq!(f.input_ack, 5);
        // Row 1 changed; ghostty also dirties the row the cursor left (row 2). Row 0 must not be.
        let rows: Vec<u16> = f.updates.iter().map(|u| u.row).collect();
        assert!(rows.contains(&1) && !rows.contains(&0), "rows: {rows:?}");
        assert_eq!(f.updates.iter().find(|u| u.row == 1).unwrap().line.text(), "B");
    }

    /// A scroll dirties every row, and a row that reads as the line the viewers hold is not
    /// sent; one changed in the same write as the scroll is, even when only its style changed
    /// and its cell's raw bits came back the same.
    #[test]
    fn a_scroll_ships_what_changed_and_keeps_what_did_not() {
        use slopty_grid::Color;
        let mut e = engine(10, 4);
        e.write(b"a\r\n\x1b[31mx\x1b[0m\r\nb\r\nc");
        let _first = e.full_frame(0).unwrap();
        assert!(e.take_frame(0).unwrap().is_none(), "nothing changed");
        // Scroll by one: nothing but the new row is sent.
        e.write(b"\r\nd");
        let f = e.take_frame(1).unwrap().unwrap();
        let sent: Vec<String> = f.updates.iter().map(|u| u.line.text()).collect();
        assert_eq!(sent, ["d"], "only the row that came in");
        let mut e = engine(10, 4);
        e.write(b"a\r\n\x1b[31mx\x1b[0m\r\nb\r\nc");
        let _first = e.full_frame(0).unwrap();
        // Rewrite the red x as a plain one, which frees red's style id, then as a blue one,
        // which takes the freed id, and scroll in the same write.
        e.write(b"\x1b[2;1Hx\x1b[34m\x1b[2;1Hx\x1b[0m\x1b[4;2H\r\nd");
        let f = e.take_frame(1).unwrap().unwrap();
        let x = f.updates.iter().find(|u| u.line.text() == "x").expect("x's row is sent");
        assert_eq!(x.line.cells[0].style.fg, Color::Palette(4));
        assert_eq!(e.full_frame(2).unwrap().updates[0].line.cells[0].style.fg, Color::Palette(4));
    }

    #[test]
    fn scrollback_keeps_absolute_numbering() {
        let mut e = engine(10, 3);
        for i in 0..10 {
            e.write(format!("line{i}\r\n").as_bytes());
        }
        let f = e.full_frame(0).unwrap();
        // 10 lines + the cursor line = 11 total; screen shows the last 3.
        assert_eq!(f.total_lines, 11);
        assert_eq!(f.first_visible_line, LineIndex(8));
        assert_eq!(f.oldest_line, LineIndex(0));
        let (start, lines) = e.lines(LineIndex(2), 3).unwrap();
        assert_eq!(start, LineIndex(2));
        assert_eq!(
            lines.iter().map(Line::text).collect::<Vec<_>>(),
            vec!["line2", "line3", "line4"]
        );
    }

    #[test]
    fn eviction_shifts_oldest_but_not_indices() {
        let mut e = GhosttyEngine::new(EngineConfig {
            size: TermSize {
                cols: 10,
                rows: 2,
                metrics: CellMetrics { cell_width: 8, cell_height: 16 },
            },
            scrollback_lines: 4,
        })
        .unwrap();
        for i in 0..20 {
            e.write(format!("l{i}\r\n").as_bytes());
        }
        let f = e.full_frame(0).unwrap();
        assert_eq!(f.epoch, 0, "no invalidation while the anchor survives");
        assert_eq!(f.total_lines, 21);
        let (start, lines) = e.lines(f.oldest_line, 100).unwrap();
        assert_eq!(start, f.oldest_line);
        // Retained lines are contiguous up to the cursor line, and the last one is the newest.
        assert_eq!(lines.last().map(Line::text), Some(String::new()));
        assert_eq!(lines[lines.len() - 2].text(), "l19");
        assert_eq!(start.0 + lines.len() as u64, 21);
    }

    #[test]
    fn alt_screen_switch_bumps_epoch_and_restores_on_return() {
        let mut e = engine(10, 3);
        e.write(b"x\r\ny\r\n");
        let before = e.full_frame(0).unwrap();
        e.write(b"\x1b[?1049h");
        let alt = e.full_frame(0).unwrap();
        assert!(alt.modes.contains(TermModes::ALT_SCREEN));
        assert_ne!(alt.epoch, before.epoch);
        assert_eq!(alt.total_lines, 3);
        e.write(b"\x1b[?1049l");
        let back = e.full_frame(0).unwrap();
        assert!(!back.modes.contains(TermModes::ALT_SCREEN));
        assert_eq!(back.total_lines, before.total_lines);
        assert_eq!(back.oldest_line, before.oldest_line);
    }

    /// The alternate screen has a numbering of its own. Leaving it gives the primary its
    /// numbering back with the prompt marks and statuses on its rows, so a client takes back
    /// the lines it held; a second trip, or one the primary was reflowed during, gets a
    /// numbering never used before.
    #[test]
    fn the_alternate_screen_gives_the_primary_its_numbering_and_marks_back() {
        let mut e = engine(20, 4);
        e.write(b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07\x1b]133;D;1\x07");
        e.write(b"\x1b]133;A\x07$ \x1b]133;B\x07vim\r\n\x1b]133;C\x07");
        let before = e.full_frame(0).unwrap();
        let marks = |f: &Frame| f.updates.iter().map(|u| u.line.mark).collect::<Vec<_>>();
        assert_eq!(
            marks(&before)[1],
            SemanticMark::Prompt { exit: Some(1), input: Some(2) },
            "{before:?}"
        );
        e.write(b"\x1b[?1049h\x1b[Hvim");
        let alt = e.take_frame(0).unwrap().expect("the alternate screen");
        assert!(alt.full && alt.epoch != before.epoch);
        e.write(b"\x1b[?1049l");
        let back = e.take_frame(0).unwrap().expect("the primary");
        assert!(back.full, "a numbering other than the last frame's");
        assert_eq!(back.epoch, before.epoch);
        assert_eq!(back.first_visible_line, before.first_visible_line);
        assert_eq!(marks(&back), marks(&before), "the marks came back");
        e.write(b"\x1b[?1049h");
        let again = e.take_frame(0).unwrap().expect("the alternate screen again");
        assert!(![before.epoch, alt.epoch].contains(&again.epoch), "{}", again.epoch);
        e.resize(TermSize { cols: 30, ..e.size() }).unwrap();
        e.write(b"\x1b[?1049l");
        let reflowed = e.full_frame(0).unwrap();
        assert!(![before.epoch, alt.epoch, again.epoch].contains(&reflowed.epoch));
    }

    /// The widest, tallest terminal there is makes a frame of that size, and one past it is
    /// refused, as the wire would refuse its frames.
    #[test]
    fn the_size_ceiling_is_the_largest_terminal() {
        let mut e = engine(slopty_grid::MAX_COLS, slopty_grid::MAX_ROWS);
        e.write(b"wide");
        let frame = e.full_frame(0).unwrap();
        assert_eq!((frame.cols, frame.rows), (slopty_grid::MAX_COLS, slopty_grid::MAX_ROWS));
        assert_eq!(frame.updates[0].line.text(), "wide");
        let metrics = CellMetrics { cell_width: 8, cell_height: 16 };
        for (cols, rows) in [(slopty_grid::MAX_COLS + 1, 1), (1, slopty_grid::MAX_ROWS + 1)] {
            let err = e.resize(TermSize { cols, rows, metrics }).unwrap_err();
            assert!(matches!(err, EngineError::InvalidSize(_)), "{cols} × {rows}: {err}");
        }
        assert_eq!(e.size().cols, slopty_grid::MAX_COLS, "a refused size changes nothing");
    }

    #[test]
    fn resize_bumps_epoch() {
        let mut e = engine(10, 3);
        let a = e.full_frame(0).unwrap();
        e.resize(TermSize {
            cols: 20,
            rows: 4,
            metrics: CellMetrics { cell_width: 8, cell_height: 16 },
        })
        .unwrap();
        let b = e.full_frame(0).unwrap();
        assert_ne!(a.epoch, b.epoch);
        assert_eq!((a.epoch, e.epoch), (0, b.epoch));
        assert_eq!((b.cols, b.rows), (20, 4));
        assert_eq!(
            e.size(),
            TermSize { cols: 20, rows: 4, metrics: CellMetrics { cell_width: 8, cell_height: 16 } }
        );
        // New metrics alone are no reflow; one of the dimensions alone is.
        let metrics = CellMetrics { cell_width: 9, cell_height: 18 };
        e.resize(TermSize { cols: 20, rows: 4, metrics }).unwrap();
        assert_eq!(e.epoch, b.epoch, "metrics only");
        e.resize(TermSize { cols: 21, rows: 4, metrics }).unwrap();
        assert_eq!(e.epoch, b.epoch + 1, "columns");
        e.resize(TermSize { cols: 21, rows: 5, metrics }).unwrap();
        assert_eq!(e.epoch, b.epoch + 2, "rows");
        e.resize(TermSize { cols: 21, rows: 5, metrics }).unwrap();
        assert_eq!(e.epoch, b.epoch + 2, "the same size again");
    }

    /// Fewer rows take blank rows off the bottom first, as every terminal does: a prompt near
    /// the top stays where it is, on the primary screen and on one parked under the alternate.
    #[test]
    fn fewer_rows_trim_blank_rows_before_scrolling_into_history() {
        let text = |f: &Frame| {
            let rows = f.updates.iter().map(|u| u.line.text().trim_end().to_owned());
            rows.filter(|t| !t.is_empty()).collect::<Vec<_>>()
        };
        let mut e = engine(20, 8);
        let rows = |e: &GhosttyEngine, rows| TermSize { rows, ..e.size() };
        e.write(b"\x1b]133;A\x07~ % \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n");
        e.write(b"\x1b]133;D;0\x07\x1b]133;A\x07~ % \x1b]133;B\x07");
        let shown = ["~ % echo hi", "hi", "~ %"];
        // Shrink, grow and shrink again: the soft keyboard coming and going.
        for n in [6, 12, 6] {
            e.resize(rows(&e, n)).unwrap();
            let f = e.full_frame(0).unwrap();
            assert_eq!(text(&f), shown, "{n} rows");
            assert_eq!((f.cursor.row, f.cursor.col), (2, 4), "{n} rows");
        }
        e.write(b"\x1b[?1049h\x1b[Hvim");
        for n in [10, 5] {
            e.resize(rows(&e, n)).unwrap();
        }
        e.write(b"\x1b[?1049l");
        let f = e.full_frame(0).unwrap();
        assert_eq!(text(&f), shown, "the primary, resized under the alternate screen");
        assert_eq!((f.cursor.row, f.cursor.col), (2, 4));
    }

    /// A shell redraws its prompt after a resize from the row it counts up to at the old width.
    /// zsh's one-row prompt that a narrower screen wraps onto two rows is redrawn from the
    /// second, below a stale copy of its head, unless the prompt was cleared first. libghostty
    /// clears it only for a shell that said it redraws (`133;A;redraw=1`; libghostty-vt's
    /// default for embedders is off), and on its own from the cursor's row only (see
    /// `ghostty/redraw.rs`); the engine clears it from its first row, at the old width, and
    /// the redraw lands where the prompt was.
    #[test]
    fn a_prompt_the_shell_redraws_is_cleared_on_resize() {
        let rows = |f: &Frame| {
            f.updates.iter().map(|u| u.line.text().trim_end().to_owned()).collect::<Vec<_>>()
        };
        let zsh = |a: &[u8], typed: &[u8]| {
            let mut e = engine(40, 6);
            let ps1 = [a, b"/var/folders/xy/T/s/repo % \x1b]133;B\x07"].concat();
            e.write(b"~ % pwd\r\n/var/folders/xy/T/s/repo\r\n");
            e.write(&ps1);
            e.write(typed);
            e.resize(TermSize { cols: 20, ..e.size() }).unwrap();
            // zle's refresh after SIGWINCH: back to its prompt's first row, clear, draw again.
            e.write(b"\r\x1b[J");
            e.write(&ps1);
            rows(&e.full_frame(0).unwrap())
        };
        let output = ["~ % pwd", "/var/folders/xy/T/s/", "repo"];
        let redrawn = ["/var/folders/xy/T/s/", "repo %"];
        let head = ["/var/folders/xy/T/s/"];
        let stale = [&output[..], &head[..], &redrawn[..]].concat();
        assert_eq!(zsh(b"\x1b]133;A\x07", b""), stale, "no `redraw`: nothing is cleared");
        let cleared = [&output[..], &redrawn[..], &[""]].concat();
        assert_eq!(zsh(b"\x1b]133;A;redraw=1\x07", b""), cleared);
        // A resize between two reads of one sequence: the engine writes nothing into it.
        let split = zsh(b"\x1b]133;A;redraw=1\x07", b"\x1b[3");
        assert_eq!(split, stale, "left to libghostty");
        // Ground is libghostty's parser state, not a guess from the bytes: an OSC cancelled by
        // CAN leaves nothing open.
        let cancelled = zsh(b"\x1b]133;A;redraw=1\x07", b"\x1b]0;x\x18");
        assert_eq!(cancelled, cleared, "a cancelled sequence is ground");

        // bash redraws only the prompt's last row: `redraw=1` would lose the rows above it.
        let bash = |a: &[u8]| {
            let mut e = engine(20, 4);
            let ps1 = [a, b"ctx\r\n$ \x1b]133;B\x07"].concat();
            e.write(&ps1);
            e.write(b"echo hi");
            e.resize(TermSize { cols: 30, ..e.size() }).unwrap();
            e.write(b"\r\x1b[K$ \x1b]133;B\x07echo hi");
            rows(&e.full_frame(0).unwrap())
        };
        assert_eq!(bash(b"\x1b]133;A;redraw=last\x07"), ["ctx", "$ echo hi", "", ""]);
        assert_eq!(bash(b"\x1b]133;A;redraw=1\x07"), ["", "$ echo hi", "", ""]);
    }

    #[test]
    fn a_zero_size_or_metric_is_refused() {
        let size = |cols, rows, cell_width, cell_height| TermSize {
            cols,
            rows,
            metrics: CellMetrics { cell_width, cell_height },
        };
        let mut e = engine(10, 3);
        for bad in [size(0, 3, 8, 16), size(10, 0, 8, 16), size(10, 3, 0, 16), size(10, 3, 8, 0)] {
            assert!(
                matches!(e.resize(bad), Err(EngineError::InvalidSize(_))),
                "{bad:?} should be refused"
            );
            assert!(
                GhosttyEngine::new(EngineConfig { size: bad, scrollback_lines: 1 }).is_err(),
                "{bad:?} should not open"
            );
        }
        assert_eq!(e.size(), size(10, 3, 8, 16), "a refused resize leaves the size");
    }

    #[test]
    fn synchronized_output_holds_frames_until_it_ends() {
        let mut e = engine(10, 3);
        let _first = e.full_frame(0).unwrap();
        e.write(b"\x1b[?2026hheld");
        assert!(e.modes().unwrap().contains(TermModes::SYNC_OUTPUT));
        assert!(e.take_frame(0).unwrap().is_none(), "held back");
        assert!(e.take_frame(0).unwrap().is_none(), "still");
        e.write(b"\x1b[?2026l");
        let f = e.take_frame(1).unwrap().expect("released");
        assert!(f.updates.iter().any(|u| u.line.text() == "held"), "{f:?}");
        assert!(e.term.viewport_active().unwrap());
    }

    /// libghostty turns synchronized output off on every resize, one that keeps the grid and
    /// changes only the cell size included (ghostty #14482), and reports the hold's end. A
    /// resize to the size the engine already has never reaches it, so it ends nothing.
    #[test]
    fn a_resize_ends_a_render_hold_and_the_same_size_does_not() {
        let at = |cols, cell_width| TermSize {
            cols,
            rows: 3,
            metrics: CellMetrics { cell_width, cell_height: 16 },
        };
        let mut e = engine(10, 3);
        let _first = e.full_frame(0).unwrap();
        e.write(b"\x1b[?2026h\x1b[Hcell");
        e.resize(e.size()).unwrap();
        assert!(e.hold_remaining().is_some(), "the same size is no resize");
        assert!(e.take_frame(0).unwrap().is_none(), "still held");

        e.resize(at(10, 9)).unwrap();
        assert_eq!(e.hold_remaining(), None, "a new cell size ends the hold");
        assert!(!e.modes().unwrap().contains(TermModes::SYNC_OUTPUT));
        let f = e.take_frame(1).unwrap().expect("released");
        let row0 = f.updates.iter().find(|u| u.row == 0).map(|u| u.line.text());
        assert_eq!(row0.as_deref(), Some("cell"), "{f:?}");

        e.write(b"\x1b[?2026h\x1b[Hgrid");
        assert!(e.take_frame(2).unwrap().is_none(), "held again");
        e.resize(at(12, 9)).unwrap();
        assert_eq!(e.hold_remaining(), None, "a new grid ends the hold");
        let f = e.full_frame(3).unwrap();
        let row0 = f.updates.iter().find(|u| u.row == 0).map(|u| u.line.text());
        assert_eq!(row0.as_deref(), Some("grid"), "{f:?}");
    }

    /// A program that finishes a frame and starts the next inside one read: the finished frame
    /// goes out, not the half-drawn one after it, and not nothing until the hold ends.
    #[test]
    fn a_hold_begun_again_in_the_same_read_ships_the_finished_frame() {
        let mut e = engine(10, 3);
        let _first = e.full_frame(0).unwrap();
        e.write(b"\x1b[?2026h\x1b[Hdone\x1b[?2026l\x1b[?2026h\x1b[Hhalf");
        let f = e.take_frame(0).unwrap().expect("the finished frame");
        let row0 = f.updates.iter().find(|u| u.row == 0).map(|u| u.line.text());
        assert_eq!(row0.as_deref(), Some("done"), "{f:?}");
        assert!(e.take_frame(0).unwrap().is_none(), "nothing more while the hold lasts");
        let remaining = e.hold_remaining().expect("a hold in force");
        assert!(remaining <= std::time::Duration::from_secs(1), "{remaining:?}");
        e.write(b"\x1b[?2026l");
        assert_eq!(e.hold_remaining(), None);
        let f = e.take_frame(0).unwrap().expect("released");
        let row0 = f.updates.iter().find(|u| u.row == 0).map(|u| u.line.text());
        assert_eq!(row0.as_deref(), Some("half"));
    }

    /// A viewer joining gets every row at the sequence number the others are at, and the
    /// others' next diff still carries what changed.
    #[test]
    fn a_joiners_frame_takes_nothing_from_the_other_viewers() {
        let mut e = engine(10, 3);
        let first = e.full_frame(0).unwrap();
        e.write(b"one");
        let Joined { frame: join, images: uploads } = e.join_frame(0).unwrap();
        assert!(join.full && uploads.is_empty());
        assert_eq!(join.seq, first.seq, "no sequence number taken");
        assert!(join.updates.iter().any(|u| u.line.text() == "one"));
        let diff = e.take_frame(0).unwrap().expect("the others' diff is still due");
        assert_eq!(diff.seq, first.seq + 1);
        assert!(diff.updates.iter().any(|u| u.line.text() == "one"), "{diff:?}");
    }

    /// Viewers joining while nobody follows the diffs take a frame that becomes what the next
    /// diff is taken against: the first key after it carries the row it changed, whether the
    /// session ran unwatched from its start or scrolled on after the last viewer left.
    #[test]
    fn the_first_key_after_a_baseline_carries_only_its_row() {
        let changed = |f: &Frame| f.updates.iter().map(|u| u.row).collect::<Vec<_>>();
        let mut e = engine(20, 6);
        e.write(b"$ ls\r\na  b  c\r\n$ ");
        let attach = e.baseline_frame(0).unwrap().frame;
        assert!(attach.full && attach.updates.len() == 6, "every row: {attach:?}");
        e.write(b"x");
        let key = e.take_frame(1).unwrap().expect("the key's echo");
        assert!(!key.full, "{key:?}");
        assert_eq!(key.seq, attach.seq + 1, "no gap after the baseline");
        assert_eq!(changed(&key), [2], "the prompt's row alone");

        // The viewer left and the screen scrolled while nobody watched; the record of what it
        // held is of other lines now.
        e.write(b"\r\none\r\ntwo\r\nthree\r\nfour\r\n$ ");
        e.discard_frame();
        let back = e.baseline_frame(0).unwrap().frame;
        assert!(back.full && back.updates.len() == 6, "every row: {back:?}");
        assert_eq!(back.seq, key.seq + 1);
        e.write(b"y");
        let key = e.take_frame(2).unwrap().expect("the key's echo");
        assert!(!key.full, "{key:?}");
        assert_eq!(changed(&key), [5], "the prompt's row alone");
        assert_eq!(key.updates[0].line.text().trim_end(), "$ y");
    }

    /// A viewer joining beside one that follows the diffs leaves the follower's record as it
    /// was: with the follower up to date, the next key carries the row it changed to both.
    #[test]
    fn the_first_key_after_a_join_beside_a_follower_carries_only_its_row() {
        let mut e = engine(20, 6);
        e.write(b"$ ls\r\na  b  c\r\n$ ");
        let follower = e.baseline_frame(0).unwrap().frame;
        let joined = e.join_frame(0).unwrap().frame;
        assert!(joined.full && joined.seq == follower.seq, "{joined:?}");
        e.write(b"x");
        let key = e.take_frame(1).unwrap().expect("the key's echo");
        assert!(!key.full, "{key:?}");
        assert_eq!(key.updates.iter().map(|u| u.row).collect::<Vec<_>>(), [2]);
    }

    /// A search of the whole terminal formatted afresh, to hold an incremental one against.
    fn whole_search(e: &GhosttyEngine, pattern: &search::Pattern, max: u32) -> search::Found {
        let rows = u32::try_from(e.total_rows().unwrap()).unwrap();
        let wraps = e.row_wraps(0, rows.saturating_sub(1)).unwrap();
        search::find(&e.plain_text().unwrap(), &wraps, pattern, LineIndex(e.base), max, e.size.cols)
    }

    /// A line the program wrote past the edge is searched as one: a hit over the wrap is found,
    /// its `len` running from its start over the end of the row onto the next. A line wrapped
    /// from the history onto the screen is found too, and found again as the screen moves on.
    #[test]
    fn a_hit_over_a_soft_wrap_is_found_with_its_cells_in_reading_order() {
        use slopty_proto::terminal::SearchMatch;
        let mut e = engine(10, 3);
        // "hello world" over a 10-column grid wraps after its tenth column, inside "world".
        e.write(b"hello world\r\n");
        let found = e.search("world", false, 10).unwrap();
        assert_eq!(found.total, 1);
        assert_eq!(found.matches, [SearchMatch { line: LineIndex(0), col: 6, len: 5 }]);
        // A blank at the wrap is kept: "abcdefghi " then "jkl" holds "i j".
        e.write(b"abcdefghi jkl\r\n");
        let found = e.search("i j", false, 10).unwrap();
        assert_eq!(found.matches, [SearchMatch { line: LineIndex(2), col: 8, len: 3 }]);
        // Scrolled up past the screen and searched incrementally, the same answers as a fresh
        // search of the whole text.
        for i in 0..20 {
            e.write(format!("row {i} over the edge\r\n").as_bytes());
        }
        let pattern = search::Pattern::new("over the edge", false).unwrap();
        let found = e.search("over the edge", false, 100).unwrap();
        assert_eq!(found.total, 20);
        assert_eq!(found, whole_search(&e, &pattern, 100));
        assert!(found.matches.iter().all(|m| m.len == 13), "{:?}", found.matches);
        e.write(b"one more over the edge\r\n");
        assert_eq!(e.search("over the edge", false, 100).unwrap(), whole_search(&e, &pattern, 100));
    }

    /// Search formats a history row once, when it first finds it scrolled up, and scans it
    /// once per needle: a find bar refreshed while a program writes costs the rows written
    /// since and the screen, not the whole history again.
    #[test]
    fn a_search_after_output_formats_and_scans_only_the_new_rows() {
        let mut e = engine(20, 3);
        for i in 0..50 {
            e.write(format!("row {i} beta\r\n").as_bytes());
        }
        assert_eq!(e.search("beta", false, 10).unwrap().total, 50);
        let held = e.history.rows(e.epoch, e.base, e.base + 48).expect("the history is held");
        assert_eq!(held.first().map(String::as_str), Some("row 0 beta"));
        e.write(b"beta again\r\nand beta\r\n");
        let hits = e.history.hits_scanned();
        let found = e.search("beta", false, 10).unwrap();
        assert_eq!(found.total, 52, "a write is seen");
        assert_eq!(e.history.hits_scanned() - hits, 2, "only the rows that scrolled up since");
        let fresh = whole_search(&e, &search::Pattern::new("beta", false).unwrap(), 10);
        assert_eq!(found, fresh, "the same answer as a search of the whole text");
        // Another needle scans the rows held again, without formatting them.
        assert_eq!(e.search("row 4", false, 100).unwrap().total, 11);
    }

    /// Rows the terminal evicts leave the search's hits and count, and a reflow starts over.
    #[test]
    fn evicted_rows_and_a_reflow_leave_the_search() {
        let mut e = engine(20, 3);
        let needle = search::Pattern::new("x", false).unwrap();
        let fresh = |e: &GhosttyEngine| whole_search(e, &needle, 5);
        for i in 0..300 {
            e.write(format!("x {i}\r\n").as_bytes());
        }
        assert_eq!(e.search("x", false, 5).unwrap(), fresh(&e));
        for i in 300..3_000 {
            e.write(format!("x {i}\r\n").as_bytes());
        }
        let found = e.search("x", false, 5).unwrap();
        assert!(e.base > 300, "rows were evicted: base {}", e.base);
        assert_eq!(found, fresh(&e));
        e.resize(TermSize { cols: 12, ..e.size }).unwrap();
        e.write(b"x after\r\n");
        assert_eq!(e.search("x", false, 5).unwrap(), fresh(&e));
    }

    /// Colours are checked only after a write that could change them, and one after it.
    #[test]
    fn colour_changes_are_seen_even_split_across_writes() {
        let mut e = engine(10, 3);
        e.write(b"\x1b]11;rgb:12/34/");
        e.write(b"56\x1b\\");
        let colours = e.drain_events().into_iter().find_map(|ev| match ev {
            EngineEvent::Colors(c) => Some(c),
            _ => None,
        });
        assert_eq!(colours.and_then(|c| c.bg), Some([0x12, 0x34, 0x56]));
        assert!(may_touch_osc_state(b"x\x1bc"));
        assert!(!may_touch_osc_state(b"plain \x1b[31m text"));
    }

    /// `OSC 22` is reported when the shape changes, split across writes too, and an unknown
    /// name keeps the last.
    #[test]
    fn a_pointer_shape_is_reported_when_it_changes() {
        let pointers = |e: &GhosttyEngine| -> Vec<PointerShape> {
            e.drain_events()
                .into_iter()
                .filter_map(|ev| match ev {
                    EngineEvent::Pointer(p) => Some(p),
                    _ => None,
                })
                .collect()
        };
        let mut e = engine(10, 3);
        e.write(b"\x1b]22;pointer\x07");
        assert_eq!(pointers(&e), [PointerShape::Pointer]);
        e.write(b"\x1b]22;pointer\x07\x1b]22;no-such-shape\x07");
        assert_eq!(pointers(&e), [], "the same shape, then an unknown one: no change");
        e.write(b"\x1b]22;col-res");
        e.write(b"ize\x1b\\");
        assert_eq!(pointers(&e), [PointerShape::ColResize], "split across two writes");
        e.write(b"\x1b]22;text\x07");
        assert_eq!(pointers(&e), [PointerShape::Text], "the I-beam asked for again");
    }

    /// An empty `OSC 22` gives the pointer back: the engine reports the I-beam the terminal
    /// starts with, so the session's pointer (the last one reported, what an attach is told
    /// while it is not the default) is the default again rather than the program's last shape.
    #[test]
    fn an_empty_pointer_shape_gives_the_pointer_back() {
        let pointers = |e: &GhosttyEngine| -> Vec<PointerShape> {
            e.drain_events()
                .into_iter()
                .filter_map(|ev| match ev {
                    EngineEvent::Pointer(p) => Some(p),
                    _ => None,
                })
                .collect()
        };
        let mut e = engine(10, 3);
        for (reset, how) in [
            (&b"\x1b]22;\x1b\\"[..], "ST"),
            (b"\x1b]22;\x07", "BEL"),
            (b"\x1b]22;", "split across two writes"),
        ] {
            e.write(b"\x1b]22;pointer\x07");
            assert_eq!(pointers(&e), [PointerShape::Pointer], "{how}: the program's shape");
            e.write(reset);
            if how.starts_with("split") {
                e.write(b"\x1b\\");
            }
            assert_eq!(pointers(&e), [PointerShape::Text], "{how}: the default again");
            assert_eq!(e.pointer, PointerShape::default(), "{how}: the session's pointer");
        }
        e.write(b"\x1b]22;\x07");
        assert_eq!(pointers(&e), [], "a reset at the default changes nothing");

        let mut fresh = engine(10, 3);
        fresh.write(b"\x1b]22;grab\x07ab\x1b]22;\x1b\\cd");
        let last = pointers(&fresh).last().copied().unwrap_or_default();
        assert_eq!(last, PointerShape::default(), "a replay that set and reset it ends at default");
    }

    #[test]
    fn the_kitty_keyboard_mode_is_on_when_any_flag_is_pushed() {
        let mut e = engine(10, 3);
        assert!(!e.modes().unwrap().contains(TermModes::KITTY_KEYBOARD));
        e.write(b"\x1b[>1u");
        assert!(e.modes().unwrap().contains(TermModes::KITTY_KEYBOARD));
        assert!(!e.modes().unwrap().contains(TermModes::KEY_RELEASES), "disambiguate alone");
        e.write(b"\x1b[>3u");
        assert!(e.modes().unwrap().contains(TermModes::KEY_RELEASES), "event types");
        e.write(b"\x1b[<u");
        e.write(b"\x1b[<u");
        assert!(!e.modes().unwrap().contains(TermModes::KITTY_KEYBOARD));
    }

    #[test]
    fn the_alternate_switch_prefix_is_a_proper_prefix_only() {
        assert_eq!(alt_prefix_of(b"abc\x1b[?10"), b"\x1b[?10");
        assert_eq!(alt_prefix_of(b"\x1b[?4"), b"\x1b[?4");
        assert_eq!(alt_prefix_of(b"\x1b[?1049h"), b"", "complete: nothing pending");
        assert_eq!(alt_prefix_of(b"x\x1b[31m"), b"", "short but not a prefix");
        assert_eq!(alt_prefix_of(b"\x1b[?1049l"), b"");
        assert_eq!(alt_prefix_of(b"plain"), b"");
    }

    #[test]
    fn link_runs_merge_one_uri_and_drop_empty_ones() {
        let mut r = LinkRuns::default();
        r.push(0, Some(b"a"), false);
        r.push(1, Some(b"a"), false);
        r.push(2, None, true);
        r.push(3, Some(b"b"), false);
        r.push(4, None, false);
        r.push(5, Some(b"c"), false);
        let runs = r.finish(6);
        let seen: Vec<(u16, u16, &str)> =
            runs.iter().map(|h| (h.col, h.len, h.uri.as_str())).collect();
        assert_eq!(seen, [(0, 3, "a"), (3, 1, "b"), (5, 1, "c")]);
        let mut r = LinkRuns::default();
        r.push(2, Some(b"a"), false);
        r.close(2);
        assert!(r.finish(6).is_empty(), "a run of no cells is no run");
    }

    /// Two links side by side are two runs, and a wide character's tail stays in its link.
    #[test]
    fn adjacent_links_are_separate_runs() {
        let mut e = engine(8, 1);
        e.write(b"\x1b]8;;http://a\x1b\\A\x1b]8;;http://b\x1b\\B\x1b]8;;\x1b\\");
        let f = e.full_frame(0).unwrap();
        let seen: Vec<(u16, u16, &str)> =
            f.updates[0].line.links.iter().map(|h| (h.col, h.len, h.uri.as_str())).collect();
        assert_eq!(seen, [(0, 1, "http://a"), (1, 1, "http://b")]);
        let mut e = engine(8, 1);
        e.write("\x1b]8;;http://w\x1b\\字\x1b]8;;\x1b\\x".as_bytes());
        let f = e.full_frame(0).unwrap();
        let seen: Vec<(u16, u16, &str)> =
            f.updates[0].line.links.iter().map(|h| (h.col, h.len, h.uri.as_str())).collect();
        assert_eq!(seen, [(0, 2, "http://w")]);
    }

    /// A horizontal wheel is buttons 6 and 7, one per column; under button-event tracking
    /// (1002) a motion is reported only while a button is down.
    #[test]
    fn horizontal_wheel_and_drag_reports() {
        let at = |action, button| MouseEvent {
            action,
            button,
            mods: Mods::empty(),
            col: 1,
            row: 1,
            px: 12,
            py: 20,
        };
        let mut e = engine(10, 3);
        e.write(b"\x1b[?1002h\x1b[?1006h");
        let mut out = Vec::new();
        e.encode_mouse(&at(MouseAction::Wheel { rows: 0, cols: 2 }, None), &mut out).unwrap();
        assert_eq!(out, b"\x1b[<66;2;2M\x1b[<66;2;2M");
        out.clear();
        e.encode_mouse(&at(MouseAction::Wheel { rows: 0, cols: -1 }, None), &mut out).unwrap();
        assert_eq!(out, b"\x1b[<67;2;2M");
        out.clear();
        let left = Some(MouseButton::Left);
        e.encode_mouse(&at(MouseAction::Motion, None), &mut out).unwrap();
        assert!(out.is_empty(), "no button down: no motion report");
        e.encode_mouse(&at(MouseAction::Press, left), &mut out).unwrap();
        e.encode_mouse(&at(MouseAction::Motion, left), &mut out).unwrap();
        e.encode_mouse(&at(MouseAction::Release, left), &mut out).unwrap();
        assert_eq!(out, b"\x1b[<0;2;2M\x1b[<32;2;2M\x1b[<0;2;2m");
        out.clear();
        e.encode_mouse(&at(MouseAction::Motion, None), &mut out).unwrap();
        assert!(out.is_empty(), "released: no motion report");
    }

    /// Every button's press, its drag and its release reach the program: the middle one too,
    /// motion without a button only under 1003, and a second press or a stray release does not
    /// leave a button counted as down.
    #[test]
    fn press_motion_and_release_follow_the_buttons_held() {
        let at = |action, button| MouseEvent {
            action,
            button,
            mods: Mods::empty(),
            col: 2,
            row: 0,
            px: 20,
            py: 4,
        };
        let mut e = engine(10, 3);
        e.write(b"\x1b[?1002h\x1b[?1006h");
        let m = e.modes().unwrap();
        assert!(m.contains(TermModes::MOUSE_DRAG) && !m.contains(TermModes::MOUSE_MOTION));
        let mut out = Vec::new();
        let middle = Some(MouseButton::Middle);
        e.encode_mouse(&at(MouseAction::Press, middle), &mut out).unwrap();
        e.encode_mouse(&at(MouseAction::Press, middle), &mut out).unwrap();
        e.encode_mouse(&at(MouseAction::Motion, middle), &mut out).unwrap();
        e.encode_mouse(&at(MouseAction::Release, middle), &mut out).unwrap();
        assert_eq!(out, b"\x1b[<1;3;1M\x1b[<1;3;1M\x1b[<33;3;1M\x1b[<1;3;1m");
        out.clear();
        e.encode_mouse(&at(MouseAction::Motion, None), &mut out).unwrap();
        assert!(out.is_empty(), "one release lets the button go, however many presses");
        e.encode_mouse(&at(MouseAction::Release, Some(MouseButton::Right)), &mut out).unwrap();
        out.clear();
        e.encode_mouse(&at(MouseAction::Motion, None), &mut out).unwrap();
        assert!(out.is_empty(), "a stray release holds nothing down");

        e.write(b"\x1b[?1003h");
        assert!(e.modes().unwrap().contains(TermModes::MOUSE_MOTION));
        e.encode_mouse(&at(MouseAction::Motion, None), &mut out).unwrap();
        assert_eq!(out, b"\x1b[<35;3;1M", "1003 reports a move with no button down");
    }

    /// A line continues the one above when that one soft-wrapped, as reflow and selection have
    /// it: not when a scroll region moved the row that wrapped away from it (libghostty's
    /// continuation flag stays on the row below), and still when a line was inserted after it.
    #[test]
    fn a_line_is_wrapped_when_the_one_above_wraps_into_it() {
        let wrapped = |e: &mut GhosttyEngine| -> Vec<bool> {
            let frame = e.full_frame(0).unwrap();
            frame.updates.iter().map(|u| u.line.flags.contains(LineFlags::WRAPPED)).collect()
        };
        let mut e = engine(5, 4);
        e.write(b"\x1b[3;1H0123456789");
        assert_eq!(wrapped(&mut e), [false, false, false, true]);
        e.write(b"\x1b[1;3r\x1b[3;1H\n\x1b[r");
        // The region scrolled the row that wrapped up, onto a blank it now wraps into; the
        // row below the region continues nothing.
        assert_eq!(wrapped(&mut e), [false, false, true, false]);
        let screen = e.total_lines().unwrap() - 4;
        let (_, lines) = e.lines(LineIndex(screen), 4).unwrap();
        let fetched: Vec<bool> =
            lines.iter().map(|l| l.flags.contains(LineFlags::WRAPPED)).collect();
        assert_eq!(fetched, [false, false, true, false], "the history's lines say the same");
        let mut e = engine(5, 4);
        e.write(b"0123456789\x1b[2;1H\x1b[L");
        assert_eq!(wrapped(&mut e), [false, true, false, false], "the line inserted continues it");
    }

    /// A row whose wrap changed while the row below it did not sends that row again, with
    /// only its flag changed; and a changed row whose row above did not change keeps the wrap
    /// it had.
    #[test]
    fn a_wrap_changed_above_a_clean_row_reaches_the_viewers() {
        let rows = |frame: Option<Frame>| -> Vec<(u16, String, bool)> {
            frame.map_or_else(Vec::new, |f| {
                f.updates
                    .iter()
                    .map(|u| {
                        let text = u.line.text().trim_end().to_owned();
                        (u.row, text, u.line.flags.contains(LineFlags::WRAPPED))
                    })
                    .collect()
            })
        };
        // The cursor stays off the rows looked at: a row it leaves is drawn again anyway.
        let mut e = engine(5, 4);
        e.write(b"0123456789\x1b[4;1H");
        let _first = e.take_frame(0).unwrap();
        e.write(b"\x1b[1;3H\x1b[K\x1b[4;1H");
        let sent = rows(e.take_frame(0).unwrap());
        assert!(sent.contains(&(0, "01".to_owned(), false)), "{sent:?}");
        assert!(
            sent.contains(&(1, "56789".to_owned(), false)),
            "the erase took the wrap off the row above: {sent:?}"
        );
        e.write(b"\x1b[2;1H01234567\x1b[1;1H");
        let _wrapped = e.take_frame(0).unwrap();
        e.write(b"\x1b[3;2Hy\x1b[1;1H");
        let sent = rows(e.take_frame(0).unwrap());
        assert!(sent.contains(&(2, "5y7".to_owned(), true)), "{sent:?}");
    }

    /// A combining mark that arrives in a later read than its letter reaches the viewers, and
    /// so does an emoji's presentation selector (fuzz: the frame kept the bare letter).
    #[test]
    fn a_mark_written_after_its_letter_is_a_frame() {
        let mut e = engine(10, 3);
        for (letter, mark, cluster) in
            [("e", "\u{301}", "e\u{301}"), ("\u{2764}", "\u{fe0f}", "\u{2764}\u{fe0f}")]
        {
            e.write(format!("\r\x1b[K{letter}").as_bytes());
            let _letter = e.take_frame(0).unwrap();
            e.write(mark.as_bytes());
            let frame = e.take_frame(0).unwrap().expect("the mark is a frame");
            assert_eq!(frame.updates[0].line.cells[0].text.as_str(), cluster);
        }
    }

    /// A caret shape set in a write of its own (zsh's `zle-line-init` after the prompt) is a
    /// frame though no cell moved, and so is a caret hidden or shown in place.
    #[test]
    fn a_caret_changed_alone_sends_a_frame() {
        let mut e = engine(10, 3);
        e.write(b"~ % ");
        let first = e.take_frame(0).unwrap().expect("the prompt");
        assert_eq!(first.cursor.shape, CursorShape::Block);
        e.write(b"\x1b[5 q");
        let frame = e.take_frame(0).unwrap().expect("the bar is a frame");
        assert_eq!(frame.cursor.shape, CursorShape::Bar);
        assert!(e.take_frame(0).unwrap().is_none(), "nothing changed");
        e.write(b"\x1b[5 q");
        assert!(e.take_frame(0).unwrap().is_none(), "the same shape again is no change");
        e.write(b"\x1b[?25l");
        let frame = e.take_frame(0).unwrap().expect("hiding the caret is a frame");
        assert!(!frame.cursor.visible);
    }

    /// The worker's reading of the pty's `termios` rides on the frames, and a change sends
    /// one though no cell moved.
    #[test]
    fn the_line_discipline_is_in_the_modes_and_sends_a_frame() {
        let mut e = engine(10, 3);
        let _first = e.take_frame(0).unwrap();
        let m = e.modes().unwrap();
        assert!(!m.intersects(TermModes::ECHO_OFF | TermModes::CANONICAL), "unknown: neither");
        assert!(e.take_frame(0).unwrap().is_none(), "nothing changed");
        e.set_line_discipline(LineDiscipline { echo: false, canonical: true });
        let frame = e.take_frame(0).unwrap().expect("the change is a frame");
        assert!(frame.modes.contains(TermModes::ECHO_OFF | TermModes::CANONICAL));
        assert!(!frame.modes.prediction_allowed());
        e.set_line_discipline(LineDiscipline { echo: false, canonical: true });
        assert!(e.take_frame(0).unwrap().is_none(), "the same reading is no change");
        e.set_line_discipline(LineDiscipline { echo: true, canonical: false });
        let frame = e.take_frame(0).unwrap().expect("echo back on");
        assert!(frame.modes.prediction_allowed());
        // zsh at its prompt (`stty -a`: `-icanon -echo`): zle echoes each key itself.
        e.set_line_discipline(LineDiscipline { echo: false, canonical: false });
        let frame = e.take_frame(0).unwrap().expect("a line editor took the tty");
        assert!(frame.modes.contains(TermModes::ECHO_OFF), "{:?}", frame.modes);
        assert!(frame.modes.prediction_allowed(), "a line editor's prompt is guessed at");
    }

    #[test]
    fn modes_reflect_dec_private_modes() {
        let mut e = engine(10, 3);
        e.write(b"\x1b[?2004h\x1b[?1004h\x1b[?1h\x1b[?25l\x1b[?1000h");
        let m = e.modes().unwrap();
        assert!(m.contains(TermModes::BRACKETED_PASTE));
        assert!(m.contains(TermModes::FOCUS_EVENTS));
        assert!(m.contains(TermModes::APP_CURSOR_KEYS));
        assert!(m.contains(TermModes::CURSOR_HIDDEN));
        assert!(m.contains(TermModes::MOUSE_TRACKING));
        assert!(!m.prediction_allowed());
    }

    #[test]
    fn key_encoding_follows_terminal_modes() {
        let mut e = engine(10, 3);
        let mut out = Vec::new();
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
        e.encode_key(&key(KeyCode::A, Some("a"), Mods::empty()), &mut out).unwrap();
        assert_eq!(out, b"a");
        out.clear();
        e.encode_key(&key(KeyCode::ArrowUp, None, Mods::empty()), &mut out).unwrap();
        assert_eq!(out, b"\x1b[A");
        out.clear();
        e.write(b"\x1b[?1h");
        e.encode_key(&key(KeyCode::ArrowUp, None, Mods::empty()), &mut out).unwrap();
        assert_eq!(out, b"\x1bOA", "DECCKM switches to SS3");
        out.clear();
        e.encode_key(&key(KeyCode::C, Some("c"), Mods::CTRL), &mut out).unwrap();
        assert_eq!(out, b"\x03");
    }

    /// A key's repeat and release reach a program only once it asks for event types (Kitty
    /// keyboard flag 2): before that a release types nothing, and a repeat types the key again.
    #[test]
    fn key_releases_reach_a_program_that_asks_for_event_types() {
        let mut e = engine(10, 3);
        let key = |action| KeyEvent {
            seq: 1,
            action,
            code: KeyCode::A,
            mods: Mods::empty(),
            consumed_mods: Mods::empty(),
            text: (action != KeyAction::Release).then(|| "a".to_owned()),
            unshifted: Some('a'),
            composing: false,
            option_as_alt: false,
        };
        let typed = |e: &mut GhosttyEngine, action| {
            let mut out = Vec::new();
            e.encode_key(&key(action), &mut out).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert_eq!(typed(&mut e, KeyAction::Release), "", "nobody asked");
        assert_eq!(typed(&mut e, KeyAction::Repeat), "a");
        // Disambiguate alone still reports no release.
        e.write(b"\x1b[>1u");
        assert_eq!(typed(&mut e, KeyAction::Release), "");
        // Disambiguate and event types: the press and repeat of a key that types text are
        // still its text; the release is reported.
        e.write(b"\x1b[=3u");
        assert_eq!(typed(&mut e, KeyAction::Press), "a");
        assert_eq!(typed(&mut e, KeyAction::Repeat), "a");
        assert_eq!(typed(&mut e, KeyAction::Release), "\x1b[97;1:3u");
        e.write(b"\x1b[<u");
        assert_eq!(typed(&mut e, KeyAction::Release), "", "popped");
    }

    /// ⌥b on a Mac client arrives as the layout's `∫` and is typed as such; when the client
    /// says the key is Alt (its text then the key without ⌥) the worker prefixes an escape.
    #[test]
    fn option_as_alt_prefixes_escape_on_the_worker() {
        let mut e = engine(10, 3);
        let mut out = Vec::new();
        let key = |text: Option<&str>, consumed_mods, option_as_alt| KeyEvent {
            seq: 1,
            action: KeyAction::Press,
            code: KeyCode::B,
            mods: Mods::ALT,
            consumed_mods,
            text: text.map(str::to_owned),
            unshifted: Some('b'),
            composing: false,
            option_as_alt,
        };
        let cases: [(Option<&str>, Mods, bool, &[u8]); 4] = [
            (Some("∫"), Mods::ALT, false, "∫".as_bytes()),
            (Some("b"), Mods::empty(), true, b"\x1bb"),
            (Some("B"), Mods::empty(), true, b"\x1bB"),
            (None, Mods::empty(), true, b"\x1bb"),
        ];
        for (text, consumed, option_as_alt, want) in cases {
            out.clear();
            e.encode_key(&key(text, consumed, option_as_alt), &mut out).unwrap();
            assert_eq!(out, want, "{text:?} {option_as_alt}");
        }
    }

    #[test]
    fn a_paste_is_safe_by_the_mode_as_it_is_now() {
        let mut e = engine(10, 3);
        assert!(e.paste_is_safe("ls").unwrap() && !e.paste_is_safe("ls\n").unwrap());
        assert!(!e.paste_is_safe("ls\r").unwrap(), "a carriage return runs it too");
        e.write(b"\x1b[?2004h");
        assert!(e.paste_is_safe("make\nrm -rf build\n").unwrap(), "bracketed: a paste");
        assert!(!e.paste_is_safe("a\x1b[201~rm\n").unwrap(), "unless it ends the bracket");
        e.write(b"\x1b[?2004l");
        assert!(!e.paste_is_safe("make\n").unwrap(), "the mode off again");
    }

    #[test]
    fn paste_is_bracketed_only_when_requested() {
        let mut e = engine(10, 3);
        let mut out = Vec::new();
        e.encode_paste("hello", &mut out).unwrap();
        assert_eq!(out, b"hello");
        out.clear();
        e.write(b"\x1b[?2004h");
        e.encode_paste("hello", &mut out).unwrap();
        assert_eq!(out, b"\x1b[200~hello\x1b[201~");
    }

    #[test]
    fn focus_only_when_enabled() {
        let mut e = engine(10, 3);
        let mut out = Vec::new();
        e.encode_focus(true, &mut out).unwrap();
        assert_eq!(out, Vec::<u8>::new());
        e.write(b"\x1b[?1004h");
        e.encode_focus(true, &mut out).unwrap();
        assert_eq!(out, b"\x1b[I");
    }

    #[test]
    fn sgr_mouse_reports_pixels_from_client_metrics() {
        let mut e = engine(10, 3);
        e.write(b"\x1b[?1000h\x1b[?1006h");
        let mut out = Vec::new();
        e.encode_mouse(
            &MouseEvent {
                action: MouseAction::Press,
                button: Some(MouseButton::Left),
                mods: Mods::empty(),
                col: 2,
                row: 1,
                px: 2 * 8 + 3,
                py: 16 + 5,
            },
            &mut out,
        )
        .unwrap();
        assert_eq!(out, b"\x1b[<0;3;2M");
    }

    /// The wheel is cursor keys on the alternate screen (mode 1007, on by default; off when
    /// reset), nothing on the primary screen (the client scrolls its own cache), and button
    /// 4 / 5 presses for a program tracking the mouse, one per row.
    #[test]
    fn the_wheel_is_arrow_keys_on_the_alternate_screen_and_presses_when_tracked() {
        let wheel = |rows: i16| MouseEvent {
            action: MouseAction::Wheel { rows, cols: 0 },
            button: None,
            mods: Mods::empty(),
            col: 1,
            row: 1,
            px: 12,
            py: 20,
        };
        let mut e = engine(10, 3);
        let mut out = Vec::new();
        e.encode_mouse(&wheel(2), &mut out).unwrap();
        assert!(out.is_empty(), "the primary screen: nothing");
        e.write(b"\x1b[?1049h");
        e.encode_mouse(&wheel(2), &mut out).unwrap();
        assert_eq!(out, b"\x1b[A\x1b[A");
        out.clear();
        e.encode_mouse(&wheel(-1), &mut out).unwrap();
        assert_eq!(out, b"\x1b[B");
        out.clear();
        e.write(b"\x1b[?1007l");
        e.encode_mouse(&wheel(1), &mut out).unwrap();
        assert!(out.is_empty(), "alternate scroll reset: nothing");
        e.write(b"\x1b[?1000h\x1b[?1006h");
        e.encode_mouse(&wheel(1), &mut out).expect("a tracked wheel encodes");
        assert_eq!(out, b"\x1b[<64;2;2M", "tracked: button 4 at the cell");
        out.clear();
        e.encode_mouse(&wheel(-2), &mut out).unwrap();
        assert_eq!(out, b"\x1b[<65;2;2M\x1b[<65;2;2M");
    }

    /// OSC 10/11/12 `?` and OSC 4: the answer is the dark theme, the one every client starts
    /// in, in xterm's `rgb:rrrr/gggg/bbbb` form.
    #[test]
    fn colour_queries_are_answered_with_the_dark_theme() {
        let mut e = engine(10, 3);
        e.write(b"\x1b]11;?\x1b\\\x1b]10;?\x1b\\\x1b]12;?\x1b\\\x1b]4;1;?\x1b\\");
        let answers: Vec<String> = e
            .drain_events()
            .into_iter()
            .filter_map(|ev| match ev {
                EngineEvent::PtyWrite(b) => Some(String::from_utf8_lossy(&b).into_owned()),
                _ => None,
            })
            .collect();
        let joined = answers.concat();
        let dark = slopty_theme::TerminalPalette::DARK;
        let xterm = |c: slopty_theme::Rgb| {
            format!("rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}", c.r, c.g, c.b)
        };
        let red = dark.ansi.get(1).copied().expect("red");
        for want in [
            format!("\x1b]11;{}", xterm(dark.bg)),
            format!("\x1b]10;{}", xterm(dark.fg)),
            format!("\x1b]12;{}", xterm(dark.cursor)),
            format!("\x1b]4;1;{}", xterm(red)),
        ] {
            assert!(joined.contains(&want), "{answers:?} lacks {want:?}");
        }
    }

    /// The driver's own colours, once set, are what the queries answer.
    /// Ground is the parser's own state: complete sequences and characters are ground, and
    /// every half of one is not, whichever introducer opened it.
    #[test]
    fn ground_is_where_the_parser_stands() {
        let ground_after = |bytes: &[u8]| {
            let mut e = engine(10, 3);
            e.write(bytes);
            e.at_ground().unwrap()
        };
        for bytes in [
            &b"hello\r\n"[..],
            b"\x1b[31mred\x1b[0m",
            b"\x1b]0;title\x07",
            b"\x1b]0;title\x1b\\",
            b"\x1bP+q544e\x1b\\",
            b"\x1b(B",
            "h\u{e9}llo \u{2500}".as_bytes(),
            b"\x1b[?1049h\x1b[H",
            b"\x1b\x1b[0m",
        ] {
            assert!(ground_after(bytes), "ground after {bytes:?}");
        }
        for bytes in [
            &b"\x1b"[..],
            b"\x1b[",
            b"\x1b[3",
            b"\x1b[?104",
            b"\x1b]0;tit",
            b"\x1b]0;title\x1b",
            b"\x1b(",
            b"\xe2\x94",
            b"\xf0\x9f\x98",
            b"\xc3",
            b"\x1bP+q",
            b"\x1b_Gx",
            b"\x1b^x",
            b"\x1bXx",
            b"\x1b\x1b",
            b"\x1b]0;half\x1b[",
            b"\x1bP+q\x1b[",
            b"\x1b[3\x1bP",
        ] {
            assert!(!ground_after(bytes), "open after {bytes:?}");
        }

        // The state carries across writes, and CAN, SUB and a new escape end what was open.
        let mut e = engine(10, 3);
        for (bytes, ground) in [
            (&b"abc\x1b["[..], false),
            (b"31m", true),
            (b"\x1b]0;half", false),
            (b"\x18", true),
            (b"\x1b[3", false),
            (b"\x1a", true),
            (b"\x1b]0;half\x1b[0m", true),
            (b"\xe2", false),
            (b"x", true),
        ] {
            e.write(bytes);
            assert_eq!(e.at_ground().unwrap(), ground, "after {bytes:?}");
        }
    }

    /// XTGETTCAP `TN` answers with the terminfo name the session's programs run under, once
    /// the worker has said which.
    #[test]
    fn xtgettcap_names_the_terminfo_entry() {
        let mut e = engine(10, 3);
        let query = b"\x1bP+q544e\x1b\\";
        e.write(query);
        assert!(!e.drain_events().iter().any(|ev| matches!(ev, EngineEvent::PtyWrite(_))));
        e.set_terminfo_name("xterm-ghostty").unwrap();
        e.write(query);
        // "xterm-ghostty" in hex.
        let answer = b"\x1bP1+r544E=787465726D2D67686F73747479\x1b\\".to_vec();
        assert_eq!(e.drain_events(), vec![EngineEvent::PtyWrite(answer)]);
        assert!(e.set_terminfo_name(&"x".repeat(129)).is_err());
    }

    /// `CSI ? 996 n` is answered from the driver's background; with mode 2031 on, a change
    /// of scheme is reported unprompted, a same-scheme change is not.
    #[test]
    fn the_colour_scheme_follows_the_drivers_background() {
        let mut e = engine(10, 3);
        let replies = |e: &mut GhosttyEngine| -> String {
            e.drain_events()
                .into_iter()
                .filter_map(|ev| match ev {
                    EngineEvent::PtyWrite(b) => Some(String::from_utf8_lossy(&b).into_owned()),
                    _ => None,
                })
                .collect()
        };
        e.write(b"\x1b[?996n");
        assert_eq!(replies(&mut e), "\x1b[?997;1n", "dark by default");
        e.set_colors(&slopty_theme::TerminalPalette::LIGHT.wire()).unwrap();
        assert_eq!(replies(&mut e), "", "nobody asked to be told");
        e.write(b"\x1b[?996n");
        assert_eq!(replies(&mut e), "\x1b[?997;2n");
        e.write(b"\x1b[?2031h");
        let mut lighter = slopty_theme::TerminalPalette::LIGHT.wire();
        lighter.bg = [0xff; 3];
        e.set_colors(&lighter).unwrap();
        assert_eq!(replies(&mut e), "", "still light: nothing to report");
        e.set_colors(&slopty_theme::TerminalPalette::DARK.wire()).unwrap();
        assert_eq!(replies(&mut e), "\x1b[?997;1n", "told of the change");
        assert!(is_light([0xff, 0xff, 0xff]) && !is_light([0x0e, 0x0f, 0x12]));
    }

    /// A full reset (RIS) returns a colour the program changed with OSC 4 to the default
    /// palette (ghostty #14480), and says so.
    #[test]
    fn a_full_reset_returns_the_palette_to_the_default() {
        let mut e = engine(10, 3);
        e.write(b"\x1b]4;2;#ff00ff\x1b\\");
        let purple =
            ColorOverrides { palette: vec![(2, [0xff, 0x00, 0xff])], ..ColorOverrides::default() };
        assert!(e.drain_events().contains(&EngineEvent::Colors(purple)));
        e.write(b"\x1bc");
        assert!(e.drain_events().contains(&EngineEvent::Colors(ColorOverrides::default())));
    }

    /// OSC 10/11/12 and OSC 4 sets are reported as the whole set of changes, once per change;
    /// a reset (OSC 104/110/111/112) reports the set without them, and RIS resets the palette
    /// but keeps the dynamic colours, as xterm does (ghostty #14480); the driver's palette
    /// changing under the program's is not a change of the program's.
    #[test]
    fn the_programs_colour_changes_are_reported_as_a_whole_set() {
        let mut e = engine(10, 3);
        let colors = |e: &mut GhosttyEngine| -> Vec<ColorOverrides> {
            e.drain_events()
                .into_iter()
                .filter_map(|ev| match ev {
                    EngineEvent::Colors(c) => Some(c),
                    _ => None,
                })
                .collect()
        };
        let bg = Some([0x28, 0x2c, 0x34]);
        e.write(b"\x1b]11;#282c34\x1b\\");
        assert_eq!(colors(&mut e), vec![ColorOverrides { bg, ..ColorOverrides::default() }]);
        e.write(b"\x1b]4;1;rgb:e0/6c/75;17;#123456\x1b\\\x1b]12;#ffffff\x1b\\");
        let red = (1, [0xe0, 0x6c, 0x75]);
        assert_eq!(
            colors(&mut e),
            vec![ColorOverrides {
                fg: None,
                bg,
                cursor: Some([0xff; 3]),
                palette: vec![red, (17, [0x12, 0x34, 0x56])],
            }]
        );
        e.write(b"\x1b]12;#ffffff\x1b\\");
        assert!(colors(&mut e).is_empty(), "setting what is set says nothing");
        e.set_colors(&slopty_theme::TerminalPalette::LIGHT.wire()).unwrap();
        e.write(b"x");
        assert!(colors(&mut e).is_empty(), "the driver's palette is under the program's");
        e.write(b"\x1b]111\x1b\\\x1b]104;17\x1b\\");
        assert_eq!(
            colors(&mut e),
            vec![ColorOverrides {
                cursor: Some([0xff; 3]),
                palette: vec![red],
                ..ColorOverrides::default()
            }]
        );
        e.write(b"\x1bc");
        assert_eq!(
            colors(&mut e),
            vec![ColorOverrides { cursor: Some([0xff; 3]), ..ColorOverrides::default() }],
            "RIS resets the palette and keeps the dynamic colours, as xterm does"
        );
        e.write(b"\x1b]112\x1b\\\x1b]104\x1b\\");
        assert_eq!(colors(&mut e), vec![ColorOverrides::default()]);
    }
    #[test]
    fn colour_queries_answer_with_the_drivers_colours_once_set() {
        let mut e = engine(10, 3);
        e.set_colors(&slopty_theme::TerminalPalette::LIGHT.wire()).unwrap();
        e.write(b"\x1b]11;?\x1b\\\x1b]4;1;?\x1b\\");
        let joined: String = e
            .drain_events()
            .into_iter()
            .filter_map(|ev| match ev {
                EngineEvent::PtyWrite(b) => Some(String::from_utf8_lossy(&b).into_owned()),
                _ => None,
            })
            .collect();
        let light = slopty_theme::TerminalPalette::LIGHT;
        let hex = |c: slopty_theme::Rgb| {
            format!("rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}", c.r, c.g, c.b)
        };
        assert!(joined.contains(&format!("\x1b]11;{}", hex(light.bg))), "{joined:?}");
        let red = light.ansi.get(1).copied().expect("red");
        assert!(joined.contains(&format!("\x1b]4;1;{}", hex(red))), "{joined:?}");
    }

    #[test]
    fn query_responses_and_osc_side_effects_are_events() {
        let mut e = engine(10, 3);
        e.write(b"\x1b[c\x07\x1b]0;hello\x07\x1b]7;file:///tmp\x07");
        let ev = e.drain_events();
        assert!(matches!(&ev[0], EngineEvent::PtyWrite(b) if b.starts_with(b"\x1b[?")));
        assert!(ev.contains(&EngineEvent::Bell));
        assert!(ev.contains(&EngineEvent::Title("hello".to_owned())));
        assert!(ev.contains(&EngineEvent::Cwd("/tmp".to_owned())));
        assert_eq!(e.drain_events(), Vec::<EngineEvent>::new());
    }

    /// CAN and SUB cancel an OSC: a title, a directory, a notification, a progress report or a
    /// clipboard write cut off by one does nothing, whole or split across reads.
    #[test]
    fn an_osc_cancelled_by_can_or_sub_has_no_effect() {
        let mut e = engine(20, 3);
        e.write(b"\x1b]0;before\x07\x1b]7;file:///tmp\x07");
        drop(e.drain_events());
        e.write(b"\x1b]0;evil\x18\x1b]7;file:///etc\x1a\x1b]9;hi\x18\x1b]777;notify;t;b\x1a");
        e.write(b"\x1b]9;4;1;50\x18\x1b]52;c;aGk=\x1a\x1b]2;split");
        e.write(b"\x18");
        assert_eq!(e.drain_events(), []);
        e.write(b"\x1b]2;after\x1b\\");
        assert_eq!(e.drain_events(), [EngineEvent::Title("after".to_owned())]);
    }

    /// A command end counts exactly when libghostty acts on its OSC, framed and cancelled as
    /// any OSC is: a title written the same way shows whether it did.
    #[test]
    fn a_command_end_counts_when_libghostty_acts_on_it() {
        let framings = [
            ("", "\x07", true),
            ("", "\x1b\\", true),
            ("", "\x1b[0m", true),
            ("\x05", "\x07", true),
            ("", "\x18\x07", false),
            ("", "\x1a\x07", false),
            ("\x18", "\x07", false),
        ];
        for (after_esc, end, acted) in framings {
            let mut e = engine(20, 3);
            e.write(format!("\x1b{after_esc}]2;t{end}").as_bytes());
            let titled = e.drain_events().contains(&EngineEvent::Title("t".to_owned()));
            e.write(b"\x1b]133;A\x07$ \x1b]133;B\x07x\r\n\x1b]133;C\x07");
            e.write(format!("\x1b{after_esc}]133;D;1;{end}").as_bytes());
            let ended = e.commands_ended() == 1;
            assert_eq!((titled, ended), (acted, acted), "{after_esc:?} … {end:?}");
        }
    }

    /// A `D` cancelled by CAN leaves the next prompt without a status; one ended by an ESC that
    /// is not ST still gives it one, as libghostty acts on such an OSC.
    #[test]
    fn a_cancelled_command_end_leaves_no_status() {
        let mut e = engine(20, 6);
        let prompt = b"\x1b]133;A\x07$ \x1b]133;B\x07";
        e.write(prompt);
        e.write(b"false\r\n\x1b]133;C\x07\x1b]133;D;1;\x18\x07");
        e.write(prompt);
        e.write(b"true\r\n\x1b]133;C\x07\x1b]133;D;0\x1b[0m");
        e.write(prompt);
        let f = e.full_frame(0).unwrap();
        let marks: Vec<SemanticMark> = f.updates.iter().map(|u| u.line.mark).collect();
        let prompt = |exit, input| SemanticMark::Prompt { exit, input };
        assert_eq!(marks[1], prompt(None, Some(2)), "the cancelled status is not taken");
        assert_eq!(marks[2], prompt(Some(0), None));
    }

    /// A full reset (RIS) clears the screen and the history, and the marks, statuses and
    /// command blocks of the rows it cleared go with them, also when the reset comes in the
    /// same read as the command it follows: the prompt after it takes no status from that
    /// command, and the old commands are not listed.
    #[test]
    fn a_full_reset_drops_the_command_state() {
        let mut e = engine(20, 6);
        e.write(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        let epoch = e.position().unwrap().epoch;
        e.write(b"false\r\n\x1b]133;C\x07\x1b]133;D;1\x07\x1bc\x1b]133;A\x07$ \x1b]133;B\x07");
        let f = e.full_frame(0).unwrap();
        assert_eq!(f.updates[0].line.mark, SemanticMark::Prompt { exit: None, input: None });
        assert_eq!(e.commands(None).unwrap(), Vec::<CommandBlock>::new());
        assert_ne!(e.position().unwrap().epoch, epoch, "the history is gone: a new numbering");
        assert_eq!(e.commands_ended(), 1);
    }

    /// After a full reset the terminal redraws no prompt until the shell says it does again,
    /// so a resize at a prompt drawn after the reset leaves it alone. A `redraw=1` from before
    /// the reset no longer counts.
    #[test]
    fn a_full_reset_forgets_that_the_shell_redraws_its_prompt() {
        let mut e = engine(20, 4);
        e.write(b"\x1b]133;A;redraw=1\x07$ \x1b]133;B\x07");
        e.write(b"\x1bcout\r\n\x1b]133;A\x07$ \x1b]133;B\x07ls");
        e.resize(TermSize { cols: 30, ..e.size() }).unwrap();
        let f = e.full_frame(0).unwrap();
        let rows: Vec<String> = f.updates.iter().map(|u| u.line.text()).collect();
        assert_eq!(rows[..2], ["out", "$ ls"]);
    }

    /// A command that resets the terminal (`reset`, `tput reset`) still ends: it was running
    /// when the reset cleared its rows, and the shell's `133;D` comes after.
    #[test]
    fn a_command_that_resets_the_terminal_still_ends() {
        let mut e = engine(20, 6);
        e.write(b"\x1b]133;A\x07$ \x1b]133;B\x07reset\r\n\x1b]133;C\x07\x1bc");
        e.write(b"\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        assert_eq!(e.commands_ended(), 1);
        let blocks = e.commands(None).unwrap();
        assert_eq!((blocks.len(), blocks[0].finished, blocks[0].exit), (1, true, Some(0)));
    }

    /// An output start whose command line (`cmdline_url`, as fish 4 writes it) is longer than
    /// any buffer is still an output start: the command runs and its end counts.
    #[test]
    fn a_long_command_line_still_starts_the_output() {
        let mut e = engine(20, 6);
        let long = "x".repeat(4096);
        e.write(b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n");
        e.write(format!("\x1b]133;C;cmdline_url={long}\x07out\r\n\x1b]133;D;0\x07").as_bytes());
        assert_eq!(e.commands_ended(), 1);
    }

    /// A write that evicts the history a command's prompt was on still ends the command: the
    /// marks are taken once the write has settled.
    #[test]
    fn a_command_whose_prompt_the_same_write_evicts_still_ends() {
        let mut e = GhosttyEngine::new(EngineConfig {
            size: TermSize {
                cols: 10,
                rows: 2,
                metrics: CellMetrics { cell_width: 8, cell_height: 16 },
            },
            scrollback_lines: 4,
        })
        .unwrap();
        let mut out = b"\x1b]133;A\x07$ \x1b]133;B\x07x\r\n\x1b]133;C\x07\x1b]133;D;3\x07".to_vec();
        for i in 0..5000 {
            out.extend_from_slice(format!("o{i}\r\n").as_bytes());
        }
        e.write(&out);
        assert_ne!(e.position().unwrap().epoch, 0, "the write evicted the anchor with the prompt");
        assert_eq!(e.commands_ended(), 1);
    }

    /// A command typed at a prompt drawn before a resize is a command: the resize renumbers
    /// the lines but keeps the prompt's block open, and bash draws no new `133;A` for it.
    #[test]
    fn a_command_typed_after_a_resize_at_the_prompt_is_tracked() {
        let mut e = engine(20, 6);
        e.write(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        e.resize(TermSize { cols: 30, ..e.size() }).unwrap();
        e.write(b"ls\r\n\x1b]133;C\x07a.rs\r\n\x1b]133;D;0\x07");
        assert_eq!(e.commands_ended(), 1);
        let blocks = e.commands(None).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!((blocks[0].command.as_str(), blocks[0].exit), ("ls", Some(0)));
    }

    /// A command that runs through a resize still ends: the reflow renumbers the lines, and
    /// its block goes on from the cursor.
    #[test]
    fn a_command_running_through_a_resize_still_ends() {
        let mut e = engine(20, 6);
        e.write(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 9\r\n\x1b]133;C\x07");
        e.resize(TermSize { cols: 30, ..e.size() }).unwrap();
        e.write(b"done\r\n\x1b]133;D;0\x07");
        assert_eq!(e.commands_ended(), 1);
        let blocks = e.commands(None).unwrap();
        assert_eq!((blocks.len(), blocks[0].finished, blocks[0].exit), (1, true, Some(0)));
    }

    /// OSC 9 (a body), OSC 777 `notify` (title and body) and OSC 99 (kitty) are one event;
    /// the fields are cut to a banner's worth.
    #[test]
    fn desktop_notifications_are_events() {
        let mut e = engine(10, 3);
        e.write(b"\x1b]9;build done\x07\x1b]777;notify;Tests;all green\x07");
        let ev = e.drain_events();
        assert!(
            ev.contains(&EngineEvent::Notification {
                title: String::new(),
                body: "build done".to_owned()
            }),
            "{ev:?}"
        );
        assert!(
            ev.contains(&EngineEvent::Notification {
                title: "Tests".to_owned(),
                body: "all green".to_owned()
            }),
            "{ev:?}"
        );
        let long = "x".repeat(2000);
        e.write(format!("\x1b]9;{long}\x07").as_bytes());
        let ev = e.drain_events();
        assert!(
            matches!(&ev[0], EngineEvent::Notification { body, .. } if body.len() == NOTIFICATION_CHARS),
            "{ev:?}"
        );
        // Bytes that are not UTF-8 reach the banner as replacement characters.
        e.write(b"\x1b]777;notify;t\xc3;\xffok\x07");
        assert_eq!(
            e.drain_events(),
            [EngineEvent::Notification {
                title: "t\u{fffd}".to_owned(),
                body: "\u{fffd}ok".to_owned()
            }]
        );
        // Kitty's: a title in chunks, one of them base64, then the body that finishes it.
        e.write(b"\x1b]99;i=1:d=0;Build\x1b\\\x1b]99;i=1:d=0:e=1;IGRvbmU=\x1b\\");
        assert_eq!(e.drain_events(), [], "not done yet");
        e.write(b"\x1b]99;i=1:p=body;All green\x1b\\");
        assert_eq!(
            e.drain_events(),
            [EngineEvent::Notification {
                title: "Build done".to_owned(),
                body: "All green".to_owned()
            }]
        );
        // Its query is answered with what is shown: a title and a body.
        e.write(b"\x1b]99;i=q:p=?;\x1b\\");
        assert_eq!(
            e.drain_events(),
            [EngineEvent::PtyWrite(b"\x1b]99;i=q:p=?;o=always:p=title,body,?\x1b\\".to_vec())]
        );
    }

    fn progress_events(e: &GhosttyEngine) -> Vec<Progress> {
        e.drain_events()
            .into_iter()
            .filter_map(|ev| match ev {
                EngineEvent::Progress(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    fn progress(state: ProgressState, percent: Option<u8>) -> Progress {
        Progress { state, percent }
    }

    /// `OSC 9;4` in each state, with the `ConEmu` rules for a missing value, reported only when
    /// it changes.
    #[test]
    fn progress_reports_are_events() {
        let mut e = engine(10, 3);
        e.write(b"\x1b]9;4;1;42\x07");
        assert_eq!(progress_events(&e), [progress(ProgressState::Set, Some(42))]);
        e.write(b"\x1b]9;4;1;42\x07");
        assert_eq!(progress_events(&e), [], "no change, no event");
        e.write(b"\x1b]9;4;2\x1b\\");
        assert_eq!(progress_events(&e), [progress(ProgressState::Error, Some(42))], "kept");
        e.write(b"\x1b]9;4;4;7\x07");
        assert_eq!(progress_events(&e), [progress(ProgressState::Paused, Some(7))]);
        e.write(b"\x1b]9;4;1\x07");
        assert_eq!(progress_events(&e), [progress(ProgressState::Set, Some(0))]);
        e.write(b"\x1b]9;4;1;250\x07");
        assert_eq!(progress_events(&e), [progress(ProgressState::Set, Some(100))], "clamped");
        e.write(b"\x1b]9;4;3\x07");
        assert_eq!(progress_events(&e), [progress(ProgressState::Indeterminate, None)]);
        e.write(b"\x1b]9;4;0\x07");
        assert_eq!(progress_events(&e), [Progress::default()]);
        e.write(b"\x1b]9;4;9\x07\x1b]9;4\x07");
        assert_eq!(progress_events(&e), [], "not a state");
    }

    /// Claude Code brackets a turn with `9;4;3;` and `9;4;0;` (a trailing separator and no
    /// value), and the bar goes with the prompt when a program never clears it.
    #[test]
    fn claude_codes_turn_bar_and_a_prompt_end_progress() {
        let mut e = engine(20, 3);
        e.write(b"\x1b]9;4;3;\x07thinking");
        assert_eq!(progress_events(&e), [progress(ProgressState::Indeterminate, None)]);
        e.write(b"\x1b]9;4;0;\x07");
        assert_eq!(progress_events(&e), [Progress::default()]);
        e.write(b"\x1b]9;4;1;60\x07\r\n\x1b]133;D;130\x07\x1b]133;A\x07$ ");
        assert_eq!(
            progress_events(&e),
            [progress(ProgressState::Set, Some(60)), Progress::default()],
            "a killed program's bar is cleared by the next prompt"
        );
        e.write(b"\x1b]133;A\x07$ ");
        assert_eq!(progress_events(&e), [], "a prompt with no bar up says nothing");
    }

    #[test]
    fn osc7_urls_become_local_paths() {
        assert_eq!(cwd_from_osc7("file:///tmp"), Some("/tmp".to_owned()));
        assert_eq!(cwd_from_osc7("file://localhost/a%20b/c"), Some("/a b/c".to_owned()));
        assert_eq!(cwd_from_osc7("file://elsewhere/tmp"), None);
        assert_eq!(cwd_from_osc7(&format!("file://{}/tmp", hostname())), Some("/tmp".to_owned()));
        assert_eq!(cwd_from_osc7("kitty-shell-cwd:///tmp"), None);
        assert_eq!(cwd_from_osc7("file://"), None);
    }

    #[test]
    fn osc8_links_become_runs_on_screen_and_in_history() {
        let mut e = engine(20, 2);
        e.write(b"a \x1b]8;;https://x.y/\x1b\\link\x1b]8;;\x1b\\ b\r\n");
        e.write("\x1b]8;;file:///t\x1b\\字\x1b]8;;\x1b\\z".as_bytes());
        let f = e.full_frame(0).unwrap();
        assert_eq!(
            f.updates[0].line.links,
            vec![Hyperlink { col: 2, len: 4, uri: "https://x.y/".to_owned() }]
        );
        assert_eq!(f.updates[0].line.link_at(3).map(|l| l.uri.as_str()), Some("https://x.y/"));
        assert_eq!(f.updates[0].line.link_at(6), None);
        assert_eq!(
            f.updates[1].line.links,
            vec![Hyperlink { col: 0, len: 2, uri: "file:///t".to_owned() }],
            "a wide character's spacer tail stays in the run"
        );
        // Scroll the first line into history and read it back through the grid-ref path.
        e.write(b"\r\n\r\n");
        let (start, lines) = e.lines(LineIndex(0), 1).unwrap();
        assert_eq!(start, LineIndex(0));
        assert_eq!(lines[0].links[0].uri, "https://x.y/");
        assert_eq!((lines[0].links[0].col, lines[0].links[0].len), (2, 4));
    }

    #[test]
    fn prompt_rows_carry_the_previous_commands_exit_status() {
        let mut e = engine(20, 6);
        let prompt = b"\x1b]133;A\x07$ \x1b]133;B\x07";
        e.write(prompt);
        e.write(b"false\r\n\x1b]133;C\x07");
        // The shell's precmd: end of the command with its status, then the next prompt.
        e.write(b"\x1b]133;D;1\x07");
        e.write(prompt);
        e.write(b"ls\r\n\x1b]133;C\x07file\r\n");
        let (d, rest) = (b"\x1b]133;D".as_slice(), b";0\x07".as_slice());
        // A mark split across two reads, followed by a blank line before the prompt.
        e.write(d);
        e.write(rest);
        e.write(b"\r\n");
        e.write(prompt);
        let f = e.full_frame(0).unwrap();
        let marks: Vec<SemanticMark> = f.updates.iter().map(|u| u.line.mark).collect();
        // The typed command starts at column 2 (after "$ "); nothing typed at the newest one.
        let prompt = |exit, input| SemanticMark::Prompt { exit, input };
        assert_eq!(marks[0], prompt(None, Some(2)), "nothing ran before it");
        assert_eq!(marks[1], prompt(Some(1), Some(2)), "adjacent prompts stay apart");
        assert_eq!(marks[2], SemanticMark::Output);
        assert_eq!(marks[3], SemanticMark::Output, "blank line the shell printed");
        assert_eq!(marks[4], prompt(Some(0), None), "status survives the gap");
        assert_eq!(marks[5], SemanticMark::Output, "never written to");
        assert!(f.updates[1].line.text().starts_with("$ "));
        // A two-row prompt right under the last one (no command ran, so no status), scrolling
        // the first row into history: the second row belongs to the block above it.
        e.write(b"\r\n\x1b]133;A\x07~\r\n> \x1b]133;B\x07");
        let f = e.full_frame(0).unwrap();
        let marks: Vec<SemanticMark> = f.updates.iter().map(|u| u.line.mark).collect();
        assert_eq!(marks[3], prompt(Some(0), None));
        assert_eq!(marks[4], prompt(None, None), "the status above is taken");
        assert_eq!(marks[5], SemanticMark::PromptContinuation { input: None });
        // The same blocks on the history path.
        e.write(b"\r\n\r\n\r\n");
        let (_, lines) = e.lines(LineIndex(0), 7).unwrap();
        let marks: Vec<SemanticMark> = lines.iter().map(|l| l.mark).collect();
        assert_eq!(marks[1], prompt(Some(1), Some(2)));
        assert_eq!(marks[4], prompt(Some(0), None));
        assert_eq!(marks[5], prompt(None, None));
        assert_eq!(marks[6], SemanticMark::PromptContinuation { input: None });
    }

    /// ⌘K: the history is erased, then zsh answers ⌃L with home + erase and redraws its
    /// prompt with the marks PS1 carries. The erased rows keep their numbers but lose their
    /// marks, so the redrawn prompt is the one start on the screen and the prompt after the
    /// next command carries its status without a stale start in between.
    #[test]
    fn a_screen_erased_in_place_drops_the_marks_of_its_rows() {
        let mut e = engine(80, 12);
        let ps1 = b"\x1b]133;A\x07\r\n\x1b[1m/tmp\x1b[0m \r\n> \x1b]133;B\x07".as_slice();
        e.write(ps1);
        e.write(b"echo hi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07");
        e.write(ps1);
        e.write(b"\x1b[3J\x1b[H\x1b[2J");
        e.write(ps1);
        let f = e.full_frame(0).unwrap();
        let marks: Vec<SemanticMark> = f.updates.iter().map(|u| u.line.mark).collect();
        let prompt = |exit| SemanticMark::Prompt { exit, input: None };
        assert_eq!(f.first_visible_line, LineIndex(0), "numbering survives an erase in place");
        assert_eq!(marks[0], prompt(None));
        assert_eq!(marks[4], SemanticMark::Output, "the old prompt's start went with its row");
        e.write(b"sleep 6\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        e.write(ps1);
        let f = e.full_frame(0).unwrap();
        let marks: Vec<SemanticMark> = f.updates.iter().map(|u| u.line.mark).collect();
        assert_eq!(marks[3], prompt(Some(0)));
        assert_eq!(marks[4], SemanticMark::PromptContinuation { input: None });
        assert_eq!(marks[5], SemanticMark::PromptContinuation { input: None });
    }

    /// zsh writes `\r\r\n` and then `133;C` as two writes (app e2e run 345). The linefeed
    /// from the input row leaves the new row a prompt continuation (libghostty's guess for
    /// shells without `k=s`), and the `C` takes it out again without touching a cell, so
    /// nothing was dirty and no frame said so until the command printed or ended: a silent
    /// `sleep` was never seen running. The `C` row is forced into the next frame.
    #[test]
    fn a_133_c_on_its_own_puts_its_row_in_a_frame() {
        let mut e = engine(80, 6);
        e.write(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 6");
        let _typed = e.take_frame(0).unwrap().expect("the prompt");
        e.write(b"\x1b[?2004l\r\r\n");
        let f = e.take_frame(0).unwrap().expect("the cursor moved");
        assert_eq!(f.cursor.row, 1);
        let rows: Vec<u16> = f.updates.iter().map(|u| u.row).collect();
        assert_eq!(rows, [1], "the row reached; the row left is as it was sent");
        assert_eq!(
            f.updates[0].line.mark,
            SemanticMark::PromptContinuation { input: None },
            "libghostty guesses a continuation until told otherwise"
        );
        e.write(b"\x1b]133;C\x07");
        let f = e.take_frame(0).unwrap().expect("the mark alone is a frame");
        assert_eq!(f.cursor.row, 1);
        let rows: Vec<(u16, SemanticMark)> =
            f.updates.iter().map(|u| (u.row, u.line.mark)).collect();
        assert_eq!(rows, vec![(1, SemanticMark::Output)]);
        assert!(e.take_frame(0).unwrap().is_none(), "forced once");
        // A `C` on a row the shell then scrolls away leaves nothing behind.
        e.write(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        let _prompt = e.take_frame(0).unwrap().expect("the prompt");
        assert!(e.take_frame(0).unwrap().is_none());
    }

    /// A prompt's status comes from the marks above it, not from its cells: a `D`, then an
    /// `A`, written on the row above a prompt the viewers hold (the cursor moved back up)
    /// change that prompt's status without dirtying its row. Each change is in the next frame.
    #[test]
    fn a_mark_above_a_sent_prompt_resends_its_status() {
        let mut e = engine(20, 4);
        e.write(b"out\r\n\x1b]133;A\x07$ \x1b]133;B\x07\r\n");
        let _prompt = e.take_frame(0).unwrap().expect("the prompt");
        let marks = |f: &Frame| -> Vec<(u16, SemanticMark)> {
            f.updates.iter().map(|u| (u.row, u.line.mark)).collect()
        };
        e.write(b"\x1b[1;1H\x1b]133;D;1\x07\x1b[3;1H");
        let f = e.take_frame(0).unwrap().expect("the status moved");
        assert_eq!(marks(&f), [(1, SemanticMark::Prompt { exit: Some(1), input: None })]);
        e.write(b"\x1b[1;1H\x1b]133;A\x07\x1b[3;1H");
        let f = e.take_frame(0).unwrap().expect("the status was taken");
        assert_eq!(
            marks(&f),
            [
                (0, SemanticMark::Prompt { exit: Some(1), input: None }),
                (1, SemanticMark::Prompt { exit: None, input: None }),
            ]
        );
        assert!(e.take_frame(0).unwrap().is_none(), "sent once");
    }

    /// Bytes captured from a real zsh with the integration loaded (synchronized output,
    /// bracketed paste, the partial-line `%` marker, `D` right before the `A` of the next
    /// prompt): the output rows stay, the prompts carry the statuses.
    #[test]
    fn captured_zsh_bytes_keep_output_rows_and_statuses() {
        let mut e = engine(80, 12);
        e.write(b"\x1b[?2026h\x1b[?25h\x1b[?2026l\r\x1b[0m\x1b[27m\x1b[24m\x1b[J\x1b]133;A\x07\r\n\x1b[1m/tmp\x1b[0m \r\n> \x1b]133;B\x07\x1b[K\x1b[?2004h");
        e.write(b"s\x08seq 1 3\x08\x08\x08\x08\x08\x08\x08\x1b[36ms\x1b[36me\x1b[36mq\x1b[39m\x1b[4C\x1b[?2004l\r\r\n\x1b]133;C\x071\r\n2\r\n3\r\n\x1b[1m\x1b[7m%\x1b[27m\x1b[1m\x1b[0m                                                                               \r \r\x1b[?2026h\x1b[?25h\x1b[?2026l\x1b]133;D;0\x07\r\x1b[0m\x1b[27m\x1b[24m\x1b[J\x1b]133;A\x07\r\n\x1b[1m/tmp\x1b[0m \r\n> \x1b]133;B\x07\x1b[K\x1b[?2004h");
        let f = e.full_frame(0).unwrap();
        let texts: Vec<String> = f.updates.iter().map(|u| u.line.text()).collect();
        assert_eq!(&texts[..7], ["", "/tmp ", "> seq 1 3", "1", "2", "3", ""]);
        e.write(b"f\x08false\x1b[?2004l\r\r\n\x1b]133;C\x07\x1b[1m\x1b[7m%\x1b[27m\x1b[1m\x1b[0m                                                                               \r \r\x1b]133;D;1\x07\r\x1b[0m\x1b[27m\x1b[24m\x1b[J\x1b]133;A\x07\r\n\x1b[1m/tmp\x1b[0m exit 1 \r\n> \x1b]133;B\x07\x1b[K\x1b[?2004h");
        let f = e.full_frame(0).unwrap();
        let rows: Vec<(String, SemanticMark)> =
            f.updates.iter().map(|u| (u.line.text(), u.line.mark)).collect();
        // A three-row prompt: the command is typed on the third row, at column 2.
        let prompt = |exit| SemanticMark::Prompt { exit, input: None };
        assert_eq!(rows[0], (String::new(), prompt(None)));
        assert_eq!(rows[1], ("/tmp ".to_owned(), SemanticMark::PromptContinuation { input: None }));
        assert_eq!(
            rows[2],
            ("> seq 1 3".to_owned(), SemanticMark::PromptContinuation { input: Some(2) })
        );
        assert_eq!(rows[3], ("1".to_owned(), SemanticMark::Output));
        assert_eq!(rows[6], (String::new(), prompt(Some(0))), "status of seq, D then A");
        assert_eq!(rows[8].0, "> false");
        assert_eq!(rows[9], (String::new(), prompt(Some(1))), "status of false");
        assert_eq!(
            rows[10],
            ("/tmp exit 1 ".to_owned(), SemanticMark::PromptContinuation { input: None })
        );
        assert_eq!(
            rows[11].1,
            SemanticMark::PromptContinuation { input: None },
            "nothing typed yet"
        );
    }

    #[test]
    fn plain_rows_carry_no_link_runs() {
        let mut e = engine(10, 2);
        e.write(b"no links");
        let f = e.full_frame(0).unwrap();
        assert!(f.updates.iter().all(|u| u.line.links.is_empty()));
    }

    #[test]
    fn osc52_writes_to_the_system_clipboard_only() {
        let mut e = engine(10, 3);
        // "hello" in base64, standard clipboard.
        e.write(b"\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(
            e.drain_events(),
            vec![EngineEvent::ClipboardWrite { text: "hello".to_owned() }]
        );
        // Primary and selection are ignored.
        e.write(b"\x1b]52;p;aGVsbG8=\x07\x1b]52;s;aGVsbG8=\x07");
        assert_eq!(e.drain_events(), vec![]);
    }

    /// A kitty clipboard write (OSC 5522) reaches the clipboard like OSC 52, and one past the
    /// session's ceiling is refused while it is still being sent, not buffered to the end.
    #[test]
    fn a_kitty_clipboard_write_is_bounded_by_the_osc52_ceiling() {
        let mut e = engine(10, 3);
        let plain = "dGV4dC9wbGFpbg=="; // "text/plain"
        e.write(b"\x1b]5522;type=write:id=a\x1b\\");
        e.write(format!("\x1b]5522;type=wdata:mime={plain};aGVsbG8=\x1b\\").as_bytes());
        e.write(b"\x1b]5522;type=wdata\x1b\\");
        let events = e.drain_events();
        assert!(events.contains(&EngineEvent::ClipboardWrite { text: "hello".to_owned() }));
        assert!(events.contains(&EngineEvent::PtyWrite(
            b"\x1b]5522;type=write:status=DONE:id=a\x1b\\".to_vec()
        )));

        // "aaa" is "YWFh": one byte past the ceiling, in chunks of 3000.
        let chunks = MAX_OSC52_BYTES / 3000 + 1;
        e.write(b"\x1b]5522;type=write:id=b\x1b\\");
        let chunk = format!("\x1b]5522;type=wdata:mime={plain};{}\x1b\\", "YWFh".repeat(1000));
        for _ in 0..chunks {
            e.write(chunk.as_bytes());
        }
        e.write(b"\x1b]5522;type=wdata\x1b\\");
        let events = e.drain_events();
        assert!(!events.iter().any(|ev| matches!(ev, EngineEvent::ClipboardWrite { .. })));
        assert!(events.contains(&EngineEvent::PtyWrite(
            b"\x1b]5522;type=write:status=EFBIG:id=b\x1b\\".to_vec()
        )));
    }

    /// A line that ends at the last column leaves the cursor waiting to wrap. A resize that
    /// leaves room after it clears the wait, so the next character follows on the same row
    /// instead of starting one (ghostty #14458, carried in aislopware/ghostty).
    #[test]
    fn a_pending_wrap_does_not_survive_a_resize_that_makes_room() {
        for (cols, rows) in [(12, vec!["123456789|X"]), (8, vec!["12345678", "9|X"])] {
            let mut e = engine(10, 4);
            e.write(b"123456789|");
            e.resize(TermSize { cols, ..e.size() }).unwrap();
            e.write(b"X");
            let f = e.full_frame(0).unwrap();
            let text: Vec<String> = f.updates.iter().map(|u| u.line.text()).collect();
            assert_eq!(text[..rows.len()], rows[..], "{cols} columns");
            assert!(text[rows.len()..].iter().all(String::is_empty), "{cols} columns: {text:?}");
        }
    }

    /// With autowrap off a full line leaves the cursor on its last cell, which the next
    /// character overwrites. A resize that makes room keeps the cursor there, as xterm does,
    /// and autowrap turned back on does not wrap mid-row (carried in aislopware/ghostty).
    #[test]
    fn without_autowrap_a_resize_keeps_the_cursor_on_the_last_cell() {
        let mut e = engine(10, 4);
        e.write(b"\x1b[?7l123456789|");
        e.resize(TermSize { cols: 12, ..e.size() }).unwrap();
        e.write(b"X\x1b[?7hY");
        let f = e.full_frame(0).unwrap();
        let text: Vec<String> = f.updates.iter().map(|u| u.line.text()).collect();
        assert_eq!(text[0], "123456789XY");
        assert!(text[1..].iter().all(String::is_empty), "{text:?}");
    }

    /// A cursor saved while waiting to wrap stays after the line through two widenings in a
    /// row. The first moves it into the blank after the line; the second reflowed that blank as
    /// if it followed the previous line, which pulled the cursor back onto the last `A`
    /// (ghostty #14478, carried in aislopware/ghostty).
    #[test]
    fn a_saved_cursor_survives_repeated_widening() {
        let mut e = engine(4, 5);
        e.write(b"abc\r\nAAA|\x1b7");
        e.resize(TermSize { cols: 5, ..e.size() }).unwrap();
        e.resize(TermSize { cols: 6, ..e.size() }).unwrap();
        e.write(b"\x1b8X");
        let f = e.full_frame(0).unwrap();
        let text: Vec<String> = f.updates.iter().map(|u| u.line.text()).collect();
        assert_eq!(text[..2], ["abc", "AAA|X"]);
        assert!(text[2..].iter().all(String::is_empty), "{text:?}");
    }

    /// A cursor saved in the blanks after a line stays after it when narrowing wraps the line.
    /// ghostty measured how far into the blanks it may go from the start of the row, not from
    /// the end of the wrapped line, and put it on the `d` (carried in aislopware/ghostty).
    #[test]
    fn a_saved_cursor_after_a_line_that_narrowing_wraps_stays_after_it() {
        let mut e = engine(8, 5);
        e.write(b"abcdef\x1b[1;8H\x1b7\x1b[4;1H");
        e.resize(TermSize { cols: 4, ..e.size() }).unwrap();
        e.write(b"\x1b8X");
        let f = e.full_frame(0).unwrap();
        let text: Vec<String> = f.updates.iter().map(|u| u.line.text()).collect();
        assert_eq!(text[..2], ["abcd", "ef X"]);
        assert!(text[2..].iter().all(String::is_empty), "{text:?}");
    }

    #[test]
    fn wide_characters_get_spacer_tails() {
        let mut e = engine(6, 1);
        e.write("字x".as_bytes());
        let f = e.full_frame(0).unwrap();
        let cells = &f.updates[0].line.cells;
        assert_eq!(cells[0].width, CellWidth::Wide);
        assert_eq!(cells[0].text.as_str(), "字");
        assert_eq!(cells[1].width, CellWidth::SpacerTail);
        assert_eq!(cells[2].text.as_str(), "x");
        assert_eq!(f.updates[0].line.text(), "字x");
    }
}

#[cfg(test)]
mod scrollback_tests {
    use slopty_proto::input::CellMetrics;
    use slopty_testkit::bench::Bench;

    use super::*;

    fn engine(scrollback_lines: u32) -> GhosttyEngine {
        GhosttyEngine::new(EngineConfig {
            size: TermSize {
                cols: 80,
                rows: 24,
                metrics: CellMetrics { cell_width: 8, cell_height: 16 },
            },
            scrollback_lines,
        })
        .unwrap()
    }

    fn write_lines(e: &mut GhosttyEngine, n: u32) {
        use std::fmt::Write as _;
        let mut out = String::new();
        for i in 0..n {
            writeln!(out, "line {i} the quick brown fox jumps over the lazy dog\r")
                .expect("string");
        }
        e.write(out.as_bytes());
    }

    /// A history row read back for `FetchLines` is the row the frame showed: styles, a
    /// wide glyph and its tail, a multi-codepoint cluster, a link.
    #[test]
    fn a_fetched_row_is_the_row_the_frame_showed() {
        let mut e = engine(1_000);
        e.write(
            "\x1b[1;31mred\x1b[0m \x1b[4mul\x1b[0m 字 e\u{301} 🇻🇳 \x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\\r\n"
                .as_bytes(),
        );
        let shown = e.full_frame(0).unwrap();
        let row = Line::clone(&shown.updates.iter().find(|u| u.row == 0).expect("row 0").line);
        let index = shown.first_visible_line;
        write_lines(&mut e, 100);
        let (start, lines) = e.lines(index, 1).unwrap();
        assert_eq!(start, index);
        assert_eq!(lines, vec![row]);
    }

    /// A range wholly below the oldest line kept is empty, not the lines after it.
    #[test]
    fn a_range_below_the_oldest_line_is_empty() {
        let mut e = engine(100);
        // A little at a time, so the numbering follows the pruning rather than starting over.
        for _ in 0..100 {
            write_lines(&mut e, 20);
        }
        let oldest = LineIndex(e.base);
        assert!(oldest.0 > 20);
        let (_, lines) = e.lines(LineIndex(0), 10).unwrap();
        assert!(lines.is_empty(), "{} lines", lines.len());
        let (start, lines) = e.lines(LineIndex(oldest.0 - 5), 10).unwrap();
        assert_eq!((start, lines.len()), (oldest, 5), "the part still kept");
    }

    #[test]
    fn the_line_limit_governs_retained_history() {
        // The 10 KB byte default would keep about one page here.
        let mut e = engine(50_000);
        write_lines(&mut e, 20_000);
        let total = e.total_lines().unwrap();
        assert!(total >= 20_000, "kept {total} rows of 20k written");
    }

    #[test]
    fn search_covers_history_and_screen_with_absolute_lines() {
        let mut e = engine(50_000);
        write_lines(&mut e, 200);
        e.write(b"needle on screen");
        let found = e.search("needle", false, 100).unwrap();
        assert_eq!(found.total, 1);
        let total = e.total_lines().unwrap();
        // The needle sits on the cursor row: the newest line.
        assert_eq!(found.matches[0].line, LineIndex(total - 1));
        assert_eq!((found.matches[0].col, found.matches[0].len), (0, 6));
        let found = e.search("line 7", false, 100).unwrap();
        // "line 7", "line 70".."line 79", "line 7x" not written beyond 199: 1 + 10 = 11.
        assert_eq!(found.total, 11);
        assert_eq!(found.matches[0].line, LineIndex(7));
        assert_eq!(e.search("", false, 10).unwrap(), search::Found::default());
    }

    /// What serving one `FetchLines` chunk (4096 rows, the worker's cap) costs from the oldest
    /// history, plain and coloured rows alike. `cargo xtask bench --filter fetch_lines_cost`
    /// runs it (MEASUREMENTS.md "history fetch").
    #[test]
    #[ignore = "measurement, run by hand"]
    fn fetch_lines_cost() {
        use std::fmt::Write as _;
        let mut e = engine(50_000);
        let mut out = String::new();
        for i in 0..50_000_u32 {
            if i % 2 == 0 {
                writeln!(out, "line {i} the quick brown fox jumps over the lazy dog\r")
            } else {
                writeln!(out, "\x1b[1;32mline {i}\x1b[0m the \x1b[4mquick\x1b[0m brown fox\r")
            }
            .expect("string");
        }
        e.write(out.as_bytes());
        let oldest = LineIndex(e.base);
        let mut fetch = Bench::new("engine.fetch_lines_cost").series("4096_rows");
        for _ in 0..10 {
            let (_, lines) = fetch.time(|| e.lines(oldest, 4096).unwrap());
            assert_eq!(lines.len(), 4096);
        }
        fetch.report().unwrap();
    }

    /// What one search costs over a full history, as the text search does it today: format the
    /// whole terminal to plain text, then scan. `cargo xtask bench --filter search_cost` runs it
    /// (MEASUREMENTS.md "search").
    #[test]
    #[ignore = "measurement, run by hand"]
    fn search_cost() {
        let bench = Bench::new("engine.search_cost");
        for lines in [1_000_u32, 10_000, 50_000] {
            let mut e = engine(lines);
            write_lines(&mut e, lines);
            let mut format = bench.series(&format!("{lines}_lines.format"));
            // What the first search adds to the format: each row's soft-wrap flag.
            let mut wraps = bench.series(&format!("{lines}_lines.wraps"));
            let mut plain = bench.series(&format!("{lines}_lines.plain"));
            let mut regex = bench.series(&format!("{lines}_lines.regex"));
            let rows = u32::try_from(e.total_rows().unwrap()).unwrap();
            for _ in 0..10 {
                let text = format.time(|| e.plain_text().unwrap());
                assert_ne!(text, "");
                let flags = wraps.time(|| e.row_wraps(0, rows - 1).unwrap());
                assert_eq!(flags.len(), rows as usize);
                let found = plain.time(|| e.search("lazy dog", false, 100).unwrap());
                assert!(found.total > 0);
                let found = regex.time(|| e.search("line [0-9]+7 ", true, 100).unwrap());
                assert!(found.total > 0);
            }
            for series in [format, wraps, plain, regex] {
                series.report().unwrap();
            }
        }
    }

    /// What a find bar's refresh costs while a program writes: the same needle searched again
    /// after each 30 lines of output, over a full 50 000-line history. `cargo xtask bench
    /// --filter search_after_output_cost` runs it (MEASUREMENTS.md, "a find bar's refresh under
    /// output").
    #[test]
    #[ignore = "measurement, run by hand"]
    fn search_after_output_cost() {
        let mut e = engine(50_000);
        write_lines(&mut e, 50_000);
        let _first = e.search("lazy dog", false, 100).unwrap();
        let mut refresh = Bench::new("engine.search_after_output_cost").series("plain");
        for round in 0..20_u32 {
            write_lines(&mut e, 30);
            e.write(format!("round {round}\r\n").as_bytes());
            let found = refresh.time(|| e.search("lazy dog", false, 100).unwrap());
            assert!(found.total > 0);
        }
        refresh.report().unwrap();
    }

    #[test]
    fn history_is_pruned_near_the_line_limit() {
        let mut e = engine(1_000);
        write_lines(&mut e, 20_000);
        let total = e.total_lines().unwrap();
        // Pruning is page-granular (a page is several hundred rows), so the count lands
        // within a page of the limit on either side; what matters is that it is bounded.
        assert!(total < 2_000, "kept {total} rows with a 1k limit");
    }
}

#[cfg(test)]
mod checkpoint_tests {
    use pretty_assertions::assert_eq;
    use slopty_grid::{StyleFlags, TermModes};
    use slopty_proto::input::CellMetrics;
    use slopty_testkit::bench::Bench;

    use super::*;

    fn engine(cols: u16, rows: u16, scrollback: u32) -> GhosttyEngine {
        GhosttyEngine::new(EngineConfig {
            size: TermSize { cols, rows, metrics: CellMetrics { cell_width: 8, cell_height: 16 } },
            scrollback_lines: scrollback,
        })
        .unwrap()
    }

    /// Text of every retained line, history then screen.
    fn all_text(e: &GhosttyEngine) -> Vec<String> {
        let total = u32::try_from(e.total_lines().unwrap()).unwrap();
        let (_start, lines) = e.lines(LineIndex(e.base), total).unwrap();
        lines.iter().map(|l| l.text().trim_end().to_owned()).collect()
    }

    /// A checkpoint replays the program's colour changes and nothing else about the colours:
    /// the driver's palette is the next driver's business, so a restored engine must not
    /// report it as the program's.
    #[test]
    fn a_checkpoint_carries_the_programs_colours_not_the_drivers() {
        let colors = |e: &mut GhosttyEngine| -> Vec<ColorOverrides> {
            e.drain_events()
                .into_iter()
                .filter_map(|ev| match ev {
                    EngineEvent::Colors(c) => Some(c),
                    _ => None,
                })
                .collect()
        };
        let mut a = engine(12, 3, 100);
        a.set_colors(&slopty_theme::TerminalPalette::LIGHT.wire()).unwrap();
        a.write(b"plain\r\n");
        let mut b = engine(12, 3, 100);
        let checkpoint = {
            let mut v = Vec::new();
            a.checkpoint(&mut v).unwrap();
            v
        };
        b.write(&checkpoint);
        assert!(colors(&mut b).is_empty(), "a light driver's palette is not a program's change");
        assert_eq!(all_text(&b), all_text(&a));
        a.write(b"\x1b]11;#282c34\x1b\\\x1b]12;#ffffff\x1b\\\x1b]4;1;#e06c75;200;#123456\x1b\\");
        let _seen = colors(&mut a);
        let checkpoint = {
            let mut v = Vec::new();
            a.checkpoint(&mut v).unwrap();
            v
        };
        let mut c = engine(12, 3, 100);
        c.write(&checkpoint);
        assert_eq!(
            colors(&mut c),
            vec![ColorOverrides {
                fg: None,
                bg: Some([0x28, 0x2c, 0x34]),
                cursor: Some([0xff; 3]),
                palette: vec![(1, [0xe0, 0x6c, 0x75]), (200, [0x12, 0x34, 0x56])],
            }]
        );
    }

    /// What a checkpoint of `a` replays into a fresh engine of its size.
    fn replayed(a: &mut GhosttyEngine) -> GhosttyEngine {
        let mut state = Vec::new();
        a.checkpoint(&mut state).unwrap();
        let mut b = engine(a.size.cols, a.size.rows, 100);
        b.write(&state);
        b
    }

    /// The screen's rows as a full frame has them.
    fn screen_lines(e: &mut GhosttyEngine) -> Vec<Line> {
        e.full_frame(0).unwrap().updates.iter().map(|u| Line::clone(&u.line)).collect()
    }

    /// Cells a program never wrote stay unstyled through a checkpoint, between and after
    /// cells it drew struck through and overlined: the formatter wrote them as spaces in the
    /// pen of the cell before them (fuzz `regressions/terminal/checkpoint-struck-blanks`).
    #[test]
    fn a_checkpoint_keeps_the_cells_between_styled_ones_unstyled() {
        let mut a = engine(20, 3, 100);
        a.write(b"\x1b[9;53ma\tb\x1b[0m\r\n\x1b[7mreverse\x1b[2;12Hreverse\x1b[0m");
        let mut b = replayed(&mut a);
        let styles = |lines: &[Line]| -> Vec<Vec<Style>> {
            lines.iter().map(|l| l.cells.iter().map(|c| c.style).collect()).collect()
        };
        assert_eq!(styles(&screen_lines(&mut b)), styles(&screen_lines(&mut a)));
    }

    /// A screen drawn on the alternate screen with the character sets a program chose there
    /// comes back as drawn, though the primary's own sets are replayed before it; and the
    /// program finds its sets where it left them on its way back to the primary (fuzz
    /// `regressions/terminal/checkpoint-lost-dec-graphics-glyph`).
    #[test]
    fn a_checkpoint_draws_the_alternate_screen_in_its_own_character_sets() {
        let mut a = engine(10, 2, 100);
        a.write(b"\x1b(0lqk\x1b[?1049h\x1b(B3h\x1b(0");
        let mut b = replayed(&mut a);
        assert_eq!(all_text(&b), all_text(&a));
        assert_eq!(all_text(&b), ["   3h", ""], "drawn from where the primary's cursor was");
        a.write(b"x\x1b[?1049lq");
        b.write(b"x\x1b[?1049lq");
        assert_eq!(all_text(&b), all_text(&a));
        assert_eq!(all_text(&b), ["\u{250c}\u{2500}\u{2510}\u{2500}", ""]);

        let mut a = engine(10, 2, 100);
        a.write(b"\x1b(0\x1b[?47h3h");
        let b = replayed(&mut a);
        assert_eq!(all_text(&b), all_text(&a));
        assert_eq!(all_text(&b), ["3\u{2424}", ""]);
    }

    /// A checkpoint taken on the alternate screen draws it with a fresh pen, not with the
    /// style, hyperlink and protection the primary's cursor had.
    #[test]
    fn a_checkpoint_draws_the_alternate_screen_with_a_fresh_pen() {
        let mut a = engine(12, 2, 100);
        a.write(b"\x1b[4;31m\x1b]8;;https://example.com\x1b\\\x1b[1\"q\x1b[?1049h\x1b[0m");
        a.write(b"\x1b]8;;\x1b\\\x1b[0\"qplain");
        let mut b = replayed(&mut a);
        assert_eq!(screen_lines(&mut b)[0].cells[0].style, Style::default());
        assert!(screen_lines(&mut b)[0].links.is_empty(), "no hyperlink");
        assert_eq!(screen_lines(&mut b), screen_lines(&mut a));
    }

    /// Hyperlinks come back through a checkpoint, each with its URI and on its cells (fuzz:
    /// every capture with an `OSC 8`).
    #[test]
    fn a_checkpoint_keeps_the_hyperlinks() {
        let mut a = engine(30, 3, 100);
        a.write(b"see \x1b]8;id=doc;https://example.com/a\x1b\\docs\x1b]8;;\x1b\\ and ");
        a.write(b"\x1b[1m\x1b]8;;https://example.com/b\x1b\\bold\x1b]8;;\x1b\\\x1b[0m\r\n");
        a.write(b"\x1b]8;;https://example.com/c\x1b\\open");
        let mut b = replayed(&mut a);
        let links = |lines: &[Line]| -> Vec<Vec<Hyperlink>> {
            lines.iter().map(|l| l.links.clone()).collect()
        };
        let (got, want) = (links(&screen_lines(&mut b)), links(&screen_lines(&mut a)));
        assert_eq!(got, want);
        assert_eq!(want.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1, 0]);
        b.write(b" more");
        a.write(b" more");
        assert_eq!(links(&screen_lines(&mut b)), links(&screen_lines(&mut a)), "the open link");
    }

    /// Soft wraps come back with a checkpoint, so the replayed screen reflows on a resize as
    /// the original does: a long line, a wide character that did not fit, a typed command
    /// past the edge, and a line written with autowrap and insert later switched off and on.
    #[test]
    fn a_checkpoint_keeps_the_soft_wraps() {
        let wraps = |lines: &[Line]| -> Vec<bool> {
            lines.iter().map(|l| l.flags.contains(LineFlags::WRAPPED)).collect()
        };
        let inputs: [&[u8]; 4] = [
            b"0123456789abcde\r\nxy",
            "012345678\u{4e2d}x".as_bytes(),
            b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\echo 0123456789",
            b"0123456789abc\r\n\x1b[?7l\x1b[4h",
        ];
        for input in inputs {
            let mut a = engine(10, 4, 100);
            a.write(input);
            let mut b = replayed(&mut a);
            let want = screen_lines(&mut a);
            assert!(wraps(&want)[1], "{input:?}: the second row continues the first");
            assert_eq!(screen_lines(&mut b), want, "{input:?}");
            for e in [&mut a, &mut b] {
                e.resize(TermSize { cols: 7, ..e.size }).unwrap();
            }
            assert_eq!(all_text(&b), all_text(&a), "{input:?} reflowed");
        }
    }

    /// What an erase, a scroll and a line shift under a coloured pen leave holds only the
    /// colour, with no style of its own: the screen and the history show it all the same, and
    /// so does a checkpoint (fuzz, a scroll under a background left the row blank).
    #[test]
    fn a_background_left_by_an_erase_or_a_scroll_shows() {
        let red = Style { bg: slopty_grid::Color::Palette(1), ..Style::DEFAULT };
        let blue = Style { bg: slopty_grid::Color::Rgb(0, 0, 80), ..Style::DEFAULT };
        let mut a = engine(6, 2, 100);
        a.write(b"\x1b[41m\x1b[K\x1b[0mab\r\n\x1b[48;2;0;0;80m\n\x1b[0m");
        let lines = screen_lines(&mut a);
        let bg = |line: &Line| line.cells.iter().map(|c| c.style).collect::<Vec<_>>();
        assert_eq!(bg(&lines[1]), vec![blue; 6], "the row a scroll brought in");
        let (_, history) = a.lines(LineIndex(a.base), 1).unwrap();
        assert_eq!(bg(&history[0])[2..], [red; 4], "past the text, in the history");
        let mut b = replayed(&mut a);
        let drawn =
            |e: &mut GhosttyEngine| -> Vec<Vec<Style>> { screen_lines(e).iter().map(bg).collect() };
        assert_eq!(drawn(&mut b), drawn(&mut a));
        // A full erase too, on either screen (fuzz: ED 2 left every row unflagged, and the
        // frames blank).
        for erase in [&b"x\x1b[41m\x1b[2J"[..], b"\x1b[?1049hx\x1b[41m\x1b[2J"] {
            let mut e = engine(6, 2, 100);
            e.write(erase);
            assert_eq!(drawn(&mut e), vec![vec![red; 6]; 2], "{erase:?}");
        }
        // And the row a line deleted, a line inserted or a reverse index at the top brings in
        // (fuzz: each reset the row after filling it, which took its flag off).
        for (shift, row) in
            [(&b"x\x1b[41m\x1b[M"[..], 1), (b"x\x1b[41m\x1b[L", 0), (b"x\x1b[41m\x1bM", 0)]
        {
            let mut e = engine(6, 2, 100);
            e.write(shift);
            assert_eq!(drawn(&mut e)[row], vec![red; 6], "{shift:?}");
        }
    }

    /// A scrolling region set before the program entered the alternate screen still holds
    /// there, and the replay of that screen's rows does not scroll inside it (fuzz, the rows
    /// past the region's bottom scrolled the ones in it away).
    #[test]
    fn a_region_set_before_the_alternate_screen_scrolls_none_of_it() {
        let mut a = engine(6, 5, 100);
        a.write(b"\x1b[2;3r\x1b[?1049h\x1b[1;1Ha\x1b[2;1Hb\x1b[3;1Hc\x1b[4;1Hd\x1b[5;1He");
        let mut b = replayed(&mut a);
        assert_eq!(all_text(&b), all_text(&a));
        assert_eq!(all_text(&a), ["a", "b", "c", "d", "e"]);
        for e in [&mut a, &mut b] {
            e.write(b"\x1b[3;1H\nx");
        }
        assert_eq!(all_text(&b), all_text(&a), "the region still scrolls rows 2 and 3");
    }

    /// Grapheme clustering (mode 2027) is on from the start and after a full reset, so an
    /// emoji sequence is one wide cell. A program that turns it off gets a cell per code point,
    /// and a checkpoint keeps it off.
    #[test]
    fn a_grapheme_cluster_is_one_wide_cell_unless_the_program_turns_it_off() {
        const FAMILY: &[u8] = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}".as_bytes();
        let col = |e: &mut GhosttyEngine| e.full_frame(0).unwrap().cursor.col;
        let mut a = engine(20, 3, 100);
        a.write(FAMILY);
        assert_eq!(col(&mut a), 2, "one wide cell");
        a.write(b"\x1bc");
        a.write(FAMILY);
        assert_eq!(col(&mut a), 2, "still, after a full reset");
        a.write(b"\x1b[?2027l\r\x1b[K");
        a.write(FAMILY);
        let off = col(&mut a);
        assert!(off > 2, "a cell per code point once off: {off}");
        let mut b = replayed(&mut a);
        for e in [&mut a, &mut b] {
            e.write(b"\r\x1b[K");
            e.write(FAMILY);
        }
        assert_eq!(col(&mut b), off, "the checkpoint keeps it off");
    }

    /// The tab stops a program set come back with a checkpoint.
    #[test]
    fn a_checkpoint_keeps_the_tab_stops() {
        let mut a = engine(20, 3, 100);
        a.write(b"\x1b[3g\x1b[5G\x1bH\x1b[11G\x1bH\r");
        let mut b = replayed(&mut a);
        for e in [&mut a, &mut b] {
            e.write(b"\ta\tb");
        }
        assert_eq!(all_text(&b), all_text(&a));
        assert_eq!(all_text(&a)[0], "    a     b");
    }

    /// A shell's prompt (`OSC 133;A`), its input (`B`), the output (`C`) and the end with a
    /// status (`D`).
    fn command(e: &mut GhosttyEngine, typed: &str, output: &str, exit: Option<u8>) {
        e.write(
            format!("\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\{typed}\r\n\x1b]133;C\x1b\\").as_bytes(),
        );
        e.write(output.as_bytes());
        if let Some(exit) = exit {
            e.write(format!("\x1b]133;D;{exit}\x1b\\").as_bytes());
        }
    }

    type Placed = (u64, (u64, u64), bool, Option<u8>);

    /// The blocks as their marks place them: the prompt's line, the output's lines, and how
    /// the command ended.
    fn blocks(e: &GhosttyEngine) -> Vec<Placed> {
        let all = e.commands(None).unwrap();
        all.into_iter().map(|c| (c.prompt_line, c.output, c.finished, c.exit)).collect()
    }

    /// The command blocks come back with a checkpoint: its prompt's and output's rows and how
    /// it ended, and the command still running, which its end then ends. Replaying them ends
    /// nothing a waiter would see.
    #[test]
    fn a_checkpoint_keeps_the_command_blocks() {
        let mut a = engine(30, 6, 100);
        command(&mut a, "false", "no\r\n", Some(1));
        a.write(b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\\r\n");
        // Output on its prompt's own row: replayed, its cells start the output (`C`), and the
        // step that leaves the cursor writing output (`D`) must not end the command.
        a.write(b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\sleep 9\x1b]133;C\x1b\\ working");
        let mut b = replayed(&mut a);
        assert_eq!(blocks(&b), blocks(&a));
        assert_eq!(a.commands(None).unwrap().len(), 2);
        assert_eq!(b.commands_ended(), 0, "the replay ended no command");
        for e in [&mut a, &mut b] {
            e.write(b"done\r\n\x1b]133;D;0\x1b\\\x1b]133;A\x1b\\$ ");
        }
        assert_eq!(blocks(&b), blocks(&a));
        assert!(b.commands(None).unwrap().iter().all(|c| c.finished));
    }

    /// Every line's prompt mark comes back with a checkpoint: the prompt rows with their
    /// statuses and where their input starts, a two-row command line, the output and the
    /// rows never written (fuzz: every capture with an `OSC 133`).
    #[test]
    fn a_checkpoint_keeps_every_lines_prompt_mark() {
        let mut a = engine(30, 8, 100);
        command(&mut a, "false", "no\r\n", Some(1));
        a.write(b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\echo 'a\r\n> b'\r\n\x1b]133;C\x1b\\a\r\nb\r\n");
        a.write(b"\x1b]133;D;0\x1b\\\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\");
        let mut b = replayed(&mut a);
        let marks =
            |lines: &[Line]| -> Vec<SemanticMark> { lines.iter().map(|l| l.mark).collect() };
        let want = marks(&screen_lines(&mut a));
        assert_eq!(marks(&screen_lines(&mut b)), want);
        assert!(matches!(want[0], SemanticMark::Prompt { exit: None, input: Some(2) }), "{want:?}");
        assert!(want.contains(&SemanticMark::Prompt { exit: Some(1), input: Some(2) }), "{want:?}");
    }

    /// The cursor writes on after a checkpoint with the content it had, whatever the row the
    /// formatter wrote last ended in: the rest of a prompt, a command line going on to a
    /// second row, or output after the command started on its prompt's row.
    #[test]
    fn a_checkpoint_keeps_what_the_cursor_writes() {
        let typed = |e: &GhosttyEngine| -> Vec<String> {
            e.commands(None).unwrap().into_iter().map(|c| c.command).collect()
        };
        let marks =
            |lines: &[Line]| -> Vec<SemanticMark> { lines.iter().map(|l| l.mark).collect() };
        let cases: [(&[u8], &[u8], Option<&str>); 3] = [
            (b"\x1b]133;A\x1b\\$", b" \x1b]133;B\x1b\\ls\r\n\x1b]133;C\x1b\\", Some("ls")),
            (
                b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\",
                b"echo 'a\r\nb'\r\n\x1b]133;C\x1b\\a\r\nb\r\n",
                Some("echo 'a\nb'"),
            ),
            // The command's text is not read from its prompt's row when its output starts
            // there too, replayed or not.
            (b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\ls\x1b]133;C\x1b\\", b" out\r\n", None),
        ];
        for (before, after, command) in cases {
            let mut a = engine(30, 6, 100);
            a.write(before);
            let mut b = replayed(&mut a);
            for e in [&mut a, &mut b] {
                e.write(after);
                e.write(b"\x1b]133;D;0\x1b\\\x1b]133;A\x1b\\$ ");
            }
            assert_eq!(typed(&b), typed(&a), "{before:?}");
            if let Some(command) = command {
                assert_eq!(typed(&a), [command], "{before:?}");
            }
            assert_eq!(marks(&screen_lines(&mut b)), marks(&screen_lines(&mut a)), "{before:?}");
        }
    }

    /// A program started from a prompt takes the alternate screen with the cursor the prompt
    /// left; replayed, the alternate screen is drawn as the program drew it, and the primary's
    /// prompt comes back on the way back.
    #[test]
    fn a_checkpoint_on_the_alternate_screen_keeps_both_screens_marks() {
        let mut a = engine(30, 6, 100);
        a.write(b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\vim\x1b[?1049hediting\r\n\x1b]133;C\x1b\\out");
        let mut b = replayed(&mut a);
        let marks =
            |lines: &[Line]| -> Vec<SemanticMark> { lines.iter().map(|l| l.mark).collect() };
        assert_eq!(marks(&screen_lines(&mut b)), marks(&screen_lines(&mut a)));
        for e in [&mut a, &mut b] {
            e.write(b"\x1b[?1049l\r\n\x1b]133;C\x1b\\done\r\n\x1b]133;D;0\x1b\\");
        }
        assert_eq!(marks(&screen_lines(&mut b)), marks(&screen_lines(&mut a)));
        assert_eq!(blocks(&b), blocks(&a));
    }

    /// A checkpoint taken while a program has the alternate screen keeps the primary's
    /// blocks for the way back.
    #[test]
    fn a_checkpoint_on_the_alternate_screen_keeps_the_primarys_command_blocks() {
        let mut a = engine(30, 6, 100);
        command(&mut a, "true", "", Some(0));
        command(&mut a, "vim", "\x1b[?1049hediting", None);
        let mut b = replayed(&mut a);
        for e in [&mut a, &mut b] {
            e.write(b"\x1b[?1049l\x1b]133;D;0\x1b\\");
        }
        assert_eq!(blocks(&b), blocks(&a));
        assert_eq!(a.commands(None).unwrap().len(), 2);
    }

    #[test]
    fn a_checkpoint_rebuilds_history_screen_cursor_and_modes() {
        let mut a = engine(12, 3, 100);
        for i in 0..5 {
            a.write(format!("line {i}\r\n").as_bytes());
        }
        a.write(b"\x1b[1;31mred\x1b[0m plain\x1b[?2004h\x1b[?1h\x1b[2;4H");
        let checkpoint = {
            let mut v = Vec::new();
            a.checkpoint(&mut v).unwrap();
            v
        };

        let mut b = engine(12, 3, 100);
        b.write(&checkpoint);

        assert_eq!(all_text(&b), all_text(&a));
        let fa = a.full_frame(0).unwrap();
        let fb = b.full_frame(0).unwrap();
        assert_eq!((fb.cursor.row, fb.cursor.col), (fa.cursor.row, fa.cursor.col));
        assert_eq!(fb.total_lines, fa.total_lines);
        let red = |f: &Frame| {
            f.updates
                .iter()
                .find(|u| u.line.text().starts_with("red"))
                .map(|u| u.line.cells[0].style)
        };
        assert_eq!(red(&fb), red(&fa));
        assert!(red(&fb).unwrap().flags.contains(StyleFlags::BOLD));
        let modes = b.modes().unwrap();
        assert!(modes.contains(TermModes::BRACKETED_PASTE), "{modes:?}");
        assert!(modes.contains(TermModes::APP_CURSOR_KEYS), "{modes:?}");
    }

    #[test]
    fn a_checkpoint_on_the_alternate_screen_carries_the_primary_too() {
        let mut a = engine(10, 2, 100);
        a.write(b"prompt$ vim\r\n");
        // The switch and the alt-screen drawing arrive in one read, as they do from a program.
        a.write(b"\x1b[?1049h\x1b[H~ editor");
        assert!(a.modes().unwrap().contains(TermModes::ALT_SCREEN));
        let checkpoint = {
            let mut v = Vec::new();
            a.checkpoint(&mut v).unwrap();
            v
        };

        let mut b = engine(10, 2, 100);
        b.write(&checkpoint);
        assert!(b.modes().unwrap().contains(TermModes::ALT_SCREEN));
        assert_eq!(all_text(&b), all_text(&a));

        // Leaving the alternate screen lands on the same primary on both.
        a.write(b"\x1b[?1049l");
        b.write(b"\x1b[?1049l");
        assert_eq!(all_text(&b), all_text(&a));
        // "prompt$ vim" wrapped at ten columns and scrolled: the history line came back too.
        assert_eq!(all_text(&b), ["prompt$ vi", "m", ""]);
    }

    #[test]
    fn a_scrolled_screen_with_blank_rows_keeps_its_history_and_cursor() {
        let mut a = engine(20, 3, 100);
        a.write(b"one\r\ntwo\r\nthree\r\nfour\r\n\r\n\x1b[A");
        let checkpoint = {
            let mut v = Vec::new();
            a.checkpoint(&mut v).unwrap();
            v
        };
        let mut b = engine(20, 3, 100);
        b.write(&checkpoint);
        assert_eq!(all_text(&b), all_text(&a));
        assert_eq!(all_text(&b), ["one", "two", "three", "four", "", ""]);
        assert_eq!((b.term.cursor_y().unwrap(), b.term.cursor_x().unwrap()), (1, 0));
    }

    #[test]
    fn a_scrolling_region_comes_back_with_the_cursor_below_it() {
        let mut a = engine(20, 4, 100);
        a.write(b"a\r\nb\r\nc\r\nd\r\ne\r\nf\x1b[1;2r\x1b[?6h\x1b[2;1Hx");
        let checkpoint = {
            let mut v = Vec::new();
            a.checkpoint(&mut v).unwrap();
            v
        };
        let mut b = engine(20, 4, 100);
        b.write(&checkpoint);
        assert_eq!(all_text(&b), all_text(&a));
        assert_eq!((a.term.cursor_y().unwrap(), a.term.cursor_x().unwrap()), (1, 1));
        assert_eq!((b.term.cursor_y().unwrap(), b.term.cursor_x().unwrap()), (1, 1));
        assert!(b.term.mode(Mode::ORIGIN).unwrap());
        // A line feed at the region's bottom scrolls the region, not the screen.
        a.write(b"\n");
        b.write(b"\n");
        assert_eq!(all_text(&b), all_text(&a));
    }

    /// What one echoed keystroke costs inside the engine: `write` of one byte, then
    /// `take_frame`, on the keystroke trace's 60×12 (the "engine+frame" stage of
    /// MEASUREMENTS.md, "the keystroke path, stage by stage") and on a full-screen 200×60. The
    /// size is in each frame series' name, since a frame's cost grows with the screen.
    /// `cargo xtask bench --filter frame_cost` runs it.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn frame_cost() {
        let bench = Bench::new("engine.frame_cost");
        let mut write = bench.series("write");
        for (cols, rows) in [(60_u16, 12_u16), (200, 60)] {
            let mut e = engine(cols, rows, 1_000);
            e.write(b"$ ");
            let _first = e.take_frame(0).unwrap();
            let mut take = bench.series(&format!("take_frame.{cols}x{rows}"));
            // The worker asks again with nothing new (a flush timer, an ack): no frame.
            let mut unchanged = bench.series(&format!("take_frame_unchanged.{cols}x{rows}"));
            // One byte into the terminal costs the same at any size: timed on the first.
            let timed_write = cols == 60;
            for i in 0..1_000_u32 {
                let byte = if i % 2 == 0 { b"x" } else { b"y" };
                if timed_write {
                    write.time(|| e.write(byte));
                } else {
                    e.write(byte);
                }
                let frame = take.time(|| e.take_frame(u64::from(i)).unwrap());
                assert!(frame.is_some(), "a typed byte dirties the row");
                let none = unchanged.time(|| e.take_frame(u64::from(i)).unwrap());
                assert!(none.is_none(), "nothing changed");
                if i % 50 == 49 {
                    e.write(b"\r\n");
                }
            }
            take.report().unwrap();
            unchanged.report().unwrap();
        }
        write.report().unwrap();
    }

    /// What a line of output costs to write by script, where grapheme clustering (mode 2027)
    /// has work to do: plain ASCII, CJK (wide), and emoji sequences (ZWJ families, flags,
    /// skin tones, a combining mark). Each sample is a 76-column line and its line break into
    /// an 80×24 screen. `cargo xtask bench --filter unicode_write_cost` runs it.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn unicode_write_cost() {
        let bench = Bench::new("engine.unicode_write_cost");
        let ascii = "the quick brown fox jumps over the lazy dog, again and again and again ok\r\n";
        let cjk = format!(
            "{}\r\n",
            "\u{6f22}\u{5b57}\u{304b}\u{306a}\u{30ab}\u{30ca}\u{d55c}\u{ae00}".repeat(4)
        );
        let sequences = [
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "\u{1F1F3}\u{1F1F1}",
            "\u{1F44D}\u{1F3FD}",
            "e\u{301}",
            "\u{2764}\u{FE0F}",
        ];
        let emoji = format!("{}\r\n", sequences.concat().repeat(5));
        for (name, line) in [("ascii", ascii.to_owned()), ("cjk", cjk), ("emoji", emoji)] {
            let mut e = engine(80, 24, 1_000);
            let mut write = bench.series(name);
            for _ in 0..500 {
                write.time(|| e.write(line.as_bytes()));
            }
            write.report().unwrap();
        }
    }

    /// What an Enter at a bottom prompt costs inside the engine: three lines of output and the
    /// next prompt scroll the screen, and `take_frame` re-reads every row to ship the four that
    /// came in, on the bench's 80×24 and on a full-screen 200×60.
    /// `cargo xtask bench --filter scroll_frame_cost` runs it.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn scroll_frame_cost() {
        let bench = Bench::new("engine.scroll_frame_cost");
        for (cols, rows) in [(80_u16, 24_u16), (200, 60)] {
            let mut e = engine(cols, rows, 10_000);
            for i in 0..u32::from(rows) * 2 {
                e.write(
                    format!("-rw-r--r--  1 me  staff  {i:>6} Sep 25 file-{i}.rs\r\n").as_bytes(),
                );
            }
            e.write(b"% ");
            let _attach = e.full_frame(0).unwrap();
            let mut take = bench.series(&format!("{cols}x{rows}"));
            for i in 1..=500_u64 {
                e.write(
                    b"ls\r\nCargo.toml  crates  docs\r\napps  vendor  xtask\r\nREADME.md\r\n% ",
                );
                let frame = take.time(|| e.take_frame(i).unwrap());
                assert!(frame.is_some_and(|f| !f.full), "a diff");
            }
            take.report().unwrap();
        }
    }

    /// What frames cost with images in the history: fifty one-cell images, placed one a line
    /// and scrolled into the history, then a line of output a frame (the graphics unchanged),
    /// and then an image sent again a frame (the graphics changed, so the frame lists the
    /// placements above the screen). 80×24, 500 frames a series.
    /// `cargo xtask bench --filter history_image_frame_cost` runs it.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn history_image_frame_cost() {
        const PIXEL: &str = "/wAA/w==";
        let bench = Bench::new("engine.history_image_frame_cost");
        let mut e = engine(80, 24, 10_000);
        for i in 1..=50 {
            e.write(
                format!("\x1b_Ga=T,f=32,s=1,v=1,i={i},q=2;{PIXEL}\x1b\\ image {i}\r\n").as_bytes(),
            );
        }
        for i in 0..30 {
            e.write(format!("line {i}\r\n").as_bytes());
        }
        let _attach = e.full_frame(0).unwrap();
        let _pixels = e.drain_images();
        let mut scroll = bench.series("scroll");
        for i in 1..=500_u64 {
            e.write(format!("output line {i}\r\n").as_bytes());
            let frame = scroll.time(|| e.take_frame(i).unwrap());
            assert!(frame.is_some_and(|f| f.images.is_empty()), "every image is above");
        }
        scroll.report().unwrap();
        let mut changed = bench.series("graphics_changed");
        for i in 501..=1_000_u64 {
            e.write(format!("\x1b_Ga=t,f=32,s=1,v=1,i=999,q=2;{PIXEL}\x1b\\").as_bytes());
            let frame = changed.time(|| e.take_frame(i).unwrap());
            assert!(frame.is_some());
            let _pixels = e.drain_images();
        }
        changed.report().unwrap();
    }

    /// How long a checkpoint takes and how big it is at 80x24 with 10 000 lines of history,
    /// the number behind the checkpoint policy in `slopty_worker::session` (recorded in
    /// MEASUREMENTS). `cargo xtask bench --filter checkpoint_cost` runs it.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn checkpoint_cost() {
        let mut e = engine(80, 24, 10_000);
        let mut out = Vec::new();
        for i in 0..10_024 {
            out.extend_from_slice(
                format!(
                    "\x1b[3{}mline {i:05}\x1b[0m the quick brown fox jumps over the lazy dog\r\n",
                    i % 8
                )
                .as_bytes(),
            );
        }
        let bench = Bench::new("engine.checkpoint_cost");
        let mut fill = bench.series("fill_engine");
        fill.time(|| {
            for chunk in out.chunks(65_536) {
                e.write(chunk);
            }
        });
        let mut raw = Terminal::new(80, 24).unwrap();
        raw.set_scrollback_max_lines(Some(10_000)).unwrap();
        let mut raw_fill = bench.series("fill_raw_vt");
        raw_fill.time(|| {
            for chunk in out.chunks(65_536) {
                raw.vt_write(chunk);
            }
        });
        let mut format = bench.series("format");
        let bytes = format.time(|| {
            let mut v = Vec::new();
            e.checkpoint(&mut v).unwrap();
            v
        });
        let mut replay = bench.series("replay_one_chunk");
        let b = replay.time(|| {
            let mut b = engine(80, 24, 10_000);
            b.write(&bytes);
            b
        });
        let mut replay_chunked = bench.series("replay_64k_chunks");
        let c = replay_chunked.time(|| {
            let mut c = engine(80, 24, 10_000);
            for chunk in bytes.chunks(65_536) {
                c.write(chunk);
            }
            c
        });
        assert_eq!(all_text(&b), all_text(&e));
        assert_eq!(all_text(&c), all_text(&e));
        eprintln!(
            "checkpoint_cost: {} history lines; fill {} bytes in 64 KiB chunks; checkpoint {} bytes",
            e.total_lines().unwrap() - 24,
            out.len(),
            bytes.len()
        );
        for series in [fill, raw_fill, format, replay, replay_chunked] {
            series.report().unwrap();
        }
    }

    /// What output dense with OSCs costs per OSC: `ls --hyperlink` (a link opened and closed
    /// around each name), a title per command, and the four prompt marks, through the engine
    /// and through libghostty alone. `cargo xtask bench --filter osc_write_cost` runs it.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn osc_write_cost() {
        const COMMANDS: usize = 200;
        const NAMES: usize = 40;
        let mut out = Vec::new();
        for i in 0..COMMANDS {
            out.extend_from_slice(b"\x1b]133;A\x07\x1b]0;~/src\x07% \x1b]133;B\x07ls\r\n");
            out.extend_from_slice(b"\x1b]133;C\x07");
            for n in 0..NAMES {
                out.extend_from_slice(
                    format!(
                        "\x1b]8;;file:///Users/me/src/slopty/crates/f{n}.rs\x1b\\f{n}.rs\x1b]8;;\x1b\\  "
                    )
                    .as_bytes(),
                );
                if n % 8 == 7 {
                    out.extend_from_slice(b"\r\n");
                }
            }
            out.extend_from_slice(format!("\x1b]133;D;{}\x07", i % 2).as_bytes());
        }
        let oscs = u64::try_from(COMMANDS * (5 + NAMES * 2)).unwrap();
        let bench = Bench::new("engine.osc_write_cost");
        let mut through_engine = bench.series("per_osc").ops(oscs);
        let mut through_vt = bench.series("per_osc_raw_vt").ops(oscs);
        for _ in 0..10 {
            let mut e = engine(80, 24, 10_000);
            through_engine.time(|| {
                for chunk in out.chunks(65_536) {
                    e.write(chunk);
                }
            });
            let mut raw = Terminal::new(80, 24).unwrap();
            raw.set_scrollback_max_lines(Some(10_000)).unwrap();
            through_vt.time(|| {
                for chunk in out.chunks(65_536) {
                    raw.vt_write(chunk);
                }
            });
        }
        through_engine.report().unwrap();
        through_vt.report().unwrap();
    }

    #[test]
    fn mode_resets_invert_every_mode_but_the_screen_switch() {
        let blob =
            b"\x1b]4;0;rgb:00/00/00\x1b\\\x1b[?1h\x1b[4h\x1b[?7l\x1b[?1049h\x1b[?25l\x1b[2;1H";
        assert_eq!(mode_resets(blob), b"\x1b[?1l\x1b[4l\x1b[?7h\x1b[?25h");
        assert_eq!(mode_resets(b"plain\x1b[31m\x1b[3;4r"), Vec::<u8>::new());
    }

    #[test]
    fn an_alternate_screen_switch_split_across_reads_still_snapshots_the_primary() {
        let mut a = engine(20, 3, 100);
        a.write(b"before\r\n");
        a.write(b"\x1b[?10");
        assert!(!a.modes().unwrap().contains(TermModes::ALT_SCREEN));
        a.write(b"49h\x1b[Halt");
        assert!(a.modes().unwrap().contains(TermModes::ALT_SCREEN));
        let checkpoint = {
            let mut v = Vec::new();
            a.checkpoint(&mut v).unwrap();
            v
        };
        let mut b = engine(20, 3, 100);
        b.write(&checkpoint);
        assert_eq!(all_text(&b), all_text(&a));
        a.write(b"\x1b[?1049l");
        b.write(b"\x1b[?1049l");
        assert_eq!(all_text(&b), ["before", "", ""]);
        assert_eq!(all_text(&b), all_text(&a));
    }

    /// A program that enters the alternate screen with one mode and leaves it with another
    /// (`?47h`, then `?1049l`) is back on the primary: DECRQM says so for every one of the
    /// three, and a checkpoint taken there replays onto the primary, not the alternate screen.
    #[test]
    fn leaving_the_alternate_screen_by_another_mode_leaves_it_everywhere() {
        let mut a = engine(20, 3, 100);
        a.write(b"primary\r\n\x1b[?47halt\x1b[?1049l");
        assert!(!a.modes().unwrap().contains(TermModes::ALT_SCREEN));
        let checkpoint = {
            let mut v = Vec::new();
            a.checkpoint(&mut v).unwrap();
            v
        };
        let mut b = engine(20, 3, 100);
        b.write(&checkpoint);
        assert!(!b.modes().unwrap().contains(TermModes::ALT_SCREEN));
        assert_eq!(all_text(&b), all_text(&a));
        drop(a.drain_events());
        a.write(b"\x1b[?47$p\x1b[?1047$p\x1b[?1049$p");
        let replies: Vec<EngineEvent> = a.drain_events();
        let reset = |m: &str| EngineEvent::PtyWrite(format!("\x1b[?{m};2$y").into_bytes());
        assert_eq!(replies, [reset("47"), reset("1047"), reset("1049")]);
    }

    #[test]
    fn a_mode_turned_off_on_the_alternate_screen_stays_off_after_replay() {
        let mut a = engine(20, 3, 100);
        a.write(b"\x1b[?1h\x1b[?1049h\x1b[?1l");
        assert!(!a.modes().unwrap().contains(TermModes::APP_CURSOR_KEYS));
        let checkpoint = {
            let mut v = Vec::new();
            a.checkpoint(&mut v).unwrap();
            v
        };
        let mut b = engine(20, 3, 100);
        b.write(&checkpoint);
        assert!(!b.modes().unwrap().contains(TermModes::APP_CURSOR_KEYS));
        b.write(b"\x1b[?1049l");
        assert!(!b.modes().unwrap().contains(TermModes::APP_CURSOR_KEYS));
    }

    #[test]
    fn a_primary_checkpoint_stays_as_the_fallback_snapshot() {
        let mut a = engine(20, 3, 100);
        a.write(b"kept\r\n");
        let _first = {
            let mut v = Vec::new();
            a.checkpoint(&mut v).unwrap();
            v
        };
        assert!(a.primary_snapshot.is_some());
    }

    #[test]
    fn the_alternate_screen_switch_is_found_mid_chunk() {
        assert_eq!(alt_enter_at(b"abc\x1b[?1049hxyz"), Some(3));
        assert_eq!(alt_enter_at(b"\x1b[?25l\x1b[?47h"), Some(6));
        assert_eq!(alt_enter_at(b"\x1b[?1049l\x1b[?2004h"), None);
        assert_eq!(alt_enter_at(b"plain"), None);
    }
}

#[cfg(test)]
mod graphics_tests {
    use pretty_assertions::assert_eq;
    use slopty_proto::input::CellMetrics;

    use super::*;

    fn engine() -> GhosttyEngine {
        GhosttyEngine::new(EngineConfig {
            size: TermSize {
                cols: 20,
                rows: 5,
                metrics: CellMetrics { cell_width: 8, cell_height: 16 },
            },
            scrollback_lines: 100,
        })
        .unwrap()
    }

    fn base64(bytes: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let at = |i: usize| chunk.get(i).copied().unwrap_or(0);
            let n = u32::from(at(0)).wrapping_shl(16)
                | u32::from(at(1)).wrapping_shl(8)
                | u32::from(at(2));
            let digit = |shift: u32| char::from(T[(n.wrapping_shr(shift) & 63) as usize]);
            let digits = [digit(18), digit(12), digit(6), digit(0)];
            let (keep, pad) = match chunk.len() {
                1 => (2, "=="),
                2 => (3, "="),
                _ => (4, ""),
            };
            out.extend(digits.iter().take(keep));
            out.push_str(pad);
        }
        out
    }

    /// A kitty transmit-and-place of `w × h` pixels in `format` (32 RGBA, 24 RGB, 100 PNG).
    fn transmit(id: u32, format: u8, w: u32, h: u32, pixels: &[u8]) -> Vec<u8> {
        format!("\x1b_Ga=T,f={format},s={w},v={h},i={id};{}\x1b\\", base64(pixels)).into_bytes()
    }

    /// An image keeps its absolute line as it scrolls into the history. While it is partly on
    /// screen the frames place it; once wholly above, a scroll alone says nothing of it (the
    /// clients move it up themselves), but a frame every viewer takes whole, one for a joiner,
    /// and one after the graphics changed list it among the placements above. Deleted there, it
    /// leaves that list.
    #[test]
    fn an_image_scrolled_into_the_history_is_listed_above_the_screen() {
        let mut e = engine();
        // 20×5; the image covers two rows from line 0.
        e.write(&transmit(1, 32, 2, 2, &[7; 16]));
        e.write(b"\x1b_Ga=p,i=1,r=2,c=2,q=2\x1b\\");
        let frame = e.take_frame(0).unwrap().expect("a frame");
        let lines: Vec<LineIndex> = frame.images.iter().map(|p| p.line).collect();
        assert!(lines.iter().all(|l| *l == LineIndex(0)), "{lines:?}");
        let _uploads = e.drain_images();

        // The placement left the cursor on its second row. Four lines down, the top one is
        // line 1, and the placement on lines 0 and 1 shows its second row.
        e.write(b"\r\n\r\n\r\n\r\n");
        let frame = e.take_frame(0).unwrap().expect("a frame");
        assert_eq!(frame.first_visible_line, LineIndex(1));
        assert!(frame.images.iter().any(|p| p.line == LineIndex(0)), "partly on screen");
        assert_eq!(frame.above, None, "a scroll alone lists nothing above");

        // Further down: wholly above, in no frame's screen, and still no list.
        e.write(b"\r\n\r\n\r\n");
        let frame = e.take_frame(0).unwrap().expect("a frame");
        assert!(frame.images.is_empty(), "{:?}", frame.images);
        assert_eq!(frame.above, None);

        // A joiner is told, and so is everyone with a frame they take whole.
        let joined = e.join_frame(0).unwrap();
        let above = joined.frame.above.expect("a joiner hears of them");
        assert_eq!(above.len(), 2, "the transmission's own placement and the one put");
        assert!(above.iter().all(|p| p.image == 1 && p.line == LineIndex(0)), "{above:?}");
        assert!(joined.images.iter().any(|u| u.id == 1), "with the pixels the joiner lacks");
        let whole = e.full_frame(0).unwrap();
        assert!(whole.above.is_some_and(|a| a.len() == 2));

        // Deleted while above: the graphics changed, and the list says so.
        // `d=A` would spare it: it deletes only what is visible on screen.
        e.write(b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\");
        let frame = e.take_frame(0).unwrap().expect("a frame");
        assert_eq!(frame.above, Some(vec![]));
    }

    #[test]
    fn a_transmitted_image_is_placed_and_uploaded_once() {
        let mut e = engine();
        let pixels = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 9, 9, 9, 128];
        e.write(&transmit(1, 32, 2, 2, &pixels));
        let frame = e.take_frame(0).unwrap().expect("a frame");
        let generation = frame.images[0].generation;
        assert_eq!(
            frame.images,
            vec![Placement {
                image: 1,
                generation,
                col: 0,
                line: LineIndex(0),
                cols: 1,
                rows: 1,
                x_offset: 0,
                y_offset: 0,
                width: 2,
                height: 2,
                source: PixelRect { x: 0, y: 0, width: 2, height: 2 },
                z: 0,
            }]
        );
        assert_eq!(
            e.drain_images(),
            vec![ImageUpload { id: 1, generation, width: 2, height: 2, rgba: pixels.to_vec() }]
        );
        // Text under it: the placement is listed again, the pixels are not sent again.
        e.write(b"\r\nhello");
        let frame = e.take_frame(0).unwrap().expect("a frame");
        assert_eq!(frame.images.len(), 1);
        assert_eq!(e.drain_images(), vec![]);
        // A client attaching holds nothing: a full frame ships them again.
        let _full = e.full_frame(0).unwrap();
        assert_eq!(e.drain_images().len(), 1);
    }

    /// A baseline ships the pixels of what it places once, ahead of it, and drops what was
    /// owed to viewers that no longer follow; the diff after it sends none again.
    #[test]
    fn a_baseline_ships_its_images_once() {
        let mut e = engine();
        e.write(&transmit(1, 32, 2, 2, &[7; 16]));
        let _first = e.take_frame(0).unwrap().expect("a frame");
        // Nobody drained this frame's upload: its viewers left before it went out.
        let joined = e.baseline_frame(0).unwrap();
        assert_eq!(joined.images.iter().map(|u| u.id).collect::<Vec<_>>(), [1], "once");
        assert_eq!(joined.frame.images.len(), 1);
        assert_eq!(e.drain_images(), vec![], "nothing left for the next frame");
        e.write(b"\r\nhello");
        let frame = e.take_frame(0).unwrap().expect("a frame");
        assert!(!frame.full && frame.images.len() == 1, "{frame:?}");
        assert_eq!(e.drain_images(), vec![], "the joiners hold its pixels");
    }

    /// A placeholder run that scrolls into the history is listed above the screen: no frame
    /// scans the history's cells, so the engine keeps the placement as each client moved it up.
    #[test]
    fn a_placeholder_run_scrolled_into_the_history_is_listed_above() {
        let mut e = engine();
        let pixels: Vec<u8> = (0..32).collect();
        let transmit =
            format!("\x1b_Ga=T,U=1,f=32,s=4,v=2,i=7,c=2,r=1,q=2;{}\x1b\\", base64(&pixels));
        e.write(transmit.as_bytes());
        e.write(
            "\x1b[38;5;7m\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}\x1b[0m".as_bytes(),
        );
        let frame = e.take_frame(0).unwrap().expect("a frame");
        assert_eq!(frame.images.iter().map(|p| p.line).collect::<Vec<_>>(), [LineIndex(0)]);
        let _pixels = e.drain_images();
        e.write(b"\r\n\r\n\r\n\r\n\r\n");
        let frame = e.take_frame(0).unwrap().expect("a frame");
        assert_eq!(frame.first_visible_line, LineIndex(1));
        assert!(frame.images.is_empty(), "{:?}", frame.images);
        assert_eq!(frame.above, None, "a scroll alone lists nothing");
        let joined = e.join_frame(0).unwrap();
        let above = joined.frame.above.expect("a joiner hears of it");
        assert_eq!(
            above.iter().map(|p| (p.image, p.line)).collect::<Vec<_>>(),
            [(7, LineIndex(0))]
        );
        assert!(joined.images.iter().any(|u| u.id == 7), "with its pixels");
    }

    /// A virtual placement (`U=1`) is shown by placeholder cells: the image id in the
    /// foreground colour, the tile in the diacritics. Two cells in a row become one placement
    /// over them, scaled to fit the placement's 2×1 grid; the cells go out blank; the pixels
    /// ship once.
    #[test]
    fn unicode_placeholders_place_the_virtual_image_by_cell() {
        let mut e = engine();
        let pixels: Vec<u8> = (0..32).collect();
        let transmit = format!("\x1b_Ga=T,U=1,f=32,s=4,v=2,i=7,c=2,r=1;{}\x1b\\", base64(&pixels));
        e.write(transmit.as_bytes());
        let frame = e.take_frame(0).unwrap().expect("a frame");
        assert_eq!(frame.images, vec![], "a virtual placement alone draws nothing");
        e.write(
            "\x1b[38;5;7m\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}\x1b[0mx".as_bytes(),
        );
        let frame = e.take_frame(0).unwrap().expect("a frame");
        let generation = frame.images.first().map_or(0, |p| p.generation);
        assert_eq!(
            frame.images,
            vec![Placement {
                image: 7,
                generation,
                col: 0,
                line: LineIndex(0),
                cols: 2,
                rows: 1,
                x_offset: 0,
                y_offset: 4,
                width: 16,
                height: 8,
                source: PixelRect { x: 0, y: 0, width: 4, height: 2 },
                z: 0,
            }]
        );
        let row = &frame.updates[0].line;
        assert_eq!(row.cells[0].text, CellText::EMPTY);
        assert_eq!(row.cells[1].text, CellText::EMPTY);
        assert_eq!(row.cells[2].text.as_str(), "x");
        assert_eq!(e.drain_images().len(), 1, "the pixels ship once");
        // Text elsewhere: the run is unchanged and still placed, nothing ships again.
        e.write(b"\r\nmore");
        let frame = e.take_frame(0).unwrap().expect("a frame");
        assert_eq!(frame.images.len(), 1);
        assert_eq!(e.drain_images(), vec![]);
        // Typing on without leaving that row leaves the placeholder row clean, and a frame
        // that visits only dirty rows would lose its run.
        e.write(b"!");
        let frame = e.take_frame(0).unwrap().expect("a frame");
        assert_eq!(frame.updates.iter().map(|u| u.row).collect::<Vec<_>>(), vec![1]);
        assert_eq!(frame.images.len(), 1);
    }

    #[test]
    fn a_placement_change_alone_makes_a_frame() {
        let mut e = engine();
        e.write(&transmit(1, 32, 1, 1, &[1, 2, 3, 4]));
        let _first = e.take_frame(0).unwrap().expect("a frame");
        assert!(e.take_frame(0).unwrap().is_none(), "nothing changed");
        // Delete every placement: no cell changes, the frame says the image is gone.
        e.write(b"\x1b_Ga=d,d=a\x1b\\");
        let frame = e.take_frame(0).unwrap().expect("a frame for the deletion");
        assert_eq!(frame.images, vec![]);
    }

    #[test]
    fn rgb_and_png_transmissions_arrive_as_rgba() {
        let mut e = engine();
        e.write(&transmit(1, 24, 1, 1, &[10, 20, 30]));
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, 1, 1);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[40, 50, 60]).unwrap();
        }
        e.write(&transmit(2, 100, 1, 1, &png_bytes));
        let frame = e.take_frame(0).unwrap().expect("a frame");
        assert_eq!(frame.images.iter().map(|p| p.image).collect::<Vec<_>>(), vec![1, 2]);
        let rgba: Vec<(u32, Vec<u8>)> =
            e.drain_images().into_iter().map(|u| (u.id, u.rgba)).collect();
        assert_eq!(rgba, vec![(1, vec![10, 20, 30, 255]), (2, vec![40, 50, 60, 255])]);
    }

    /// `bytes` as a zlib stream of one stored (uncompressed) deflate block.
    fn zlib_stored(bytes: &[u8]) -> Vec<u8> {
        let len = u16::try_from(bytes.len()).unwrap();
        let (a, b) = bytes.iter().fold((1_u32, 0_u32), |(a, b), &byte| {
            let a = a.wrapping_add(u32::from(byte)) % 65_521;
            (a, b.wrapping_add(a) % 65_521)
        });
        let mut out = vec![0x78, 0x01, 0x01];
        out.extend(len.to_le_bytes());
        out.extend((!len).to_le_bytes());
        out.extend(bytes);
        out.extend((b.wrapping_shl(16) | a).to_be_bytes());
        out
    }

    /// Where a program on this machine left an image: a file (`t=f`) is read and left where
    /// it is; a temporary file (`t=t`) is read from the temporary directory and deleted. One
    /// that only claims to be temporary, outside it, is refused and kept, and so is a device.
    #[test]
    fn an_image_may_come_as_a_file_or_a_temporary_file() {
        let pixels: Vec<u8> = (0..16).collect();
        let at = |dir: &std::path::Path| {
            std::fs::create_dir_all(dir).unwrap();
            let (file, temporary) =
                (dir.join("image.rgba"), dir.join("tty-graphics-protocol.rgba"));
            std::fs::write(&file, &pixels).unwrap();
            std::fs::write(&temporary, &pixels).unwrap();
            (file, temporary)
        };
        let pid = std::process::id();
        let inside = std::env::temp_dir().join(format!("slopty-kitty-media-{pid}"));
        let outside = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../target/kitty-media-{pid}"));
        let (file, temporary) = at(&inside);
        let (_, misplaced) = at(&outside);
        let from = |medium: char, id: u32, path: &std::path::Path| {
            let path = base64(path.to_str().unwrap().as_bytes());
            format!("\x1b_Ga=T,t={medium},f=32,s=2,v=2,i={id},q=2;{path}\x1b\\")
        };
        let mut e = engine();
        e.write(from('f', 1, &file).as_bytes());
        e.write(from('t', 2, &temporary).as_bytes());
        e.write(from('t', 3, &misplaced).as_bytes());
        e.write(from('f', 4, std::path::Path::new("/dev/zero")).as_bytes());
        let _frame = e.take_frame(0).unwrap();
        let mut got: Vec<(u32, Vec<u8>)> =
            e.drain_images().into_iter().map(|u| (u.id, u.rgba)).collect();
        got.sort();
        assert_eq!(got, vec![(1, pixels.clone()), (2, pixels)]);
        assert!(file.exists(), "a file is the program's own");
        assert!(!temporary.exists(), "a temporary file is gone once read");
        assert!(misplaced.exists(), "a file outside the temporary directory is not touched");
        std::fs::remove_dir_all(&inside).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    /// Output that names a FIFO as an image file is refused at once. Opened blocking, it would
    /// wait for a writer that never comes and stall the session for good (ghostty fork #12).
    #[test]
    fn an_image_named_as_a_fifo_never_blocks_the_engine() {
        let dir = std::env::temp_dir().join(format!("slopty-kitty-fifo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("tty-graphics-protocol.fifo");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(made.success());
        let name = base64(fifo.to_str().unwrap().as_bytes());
        let (tx, rx) = std::sync::mpsc::channel();
        let _writer = std::thread::spawn(move || {
            let mut e = engine();
            for medium in ['f', 't'] {
                e.write(format!("\x1b_Ga=T,t={medium},f=24,s=1,v=1,i=1;{name}\x1b\\").as_bytes());
            }
            let _frame = e.take_frame(0).unwrap();
            tx.send(e.drain_images().len()).unwrap();
        });
        let shown = rx.recv_timeout(std::time::Duration::from_secs(10));
        if shown.is_err() {
            // Free the stuck open before failing, so the thread ends with the test.
            let _unblocked = std::fs::OpenOptions::new().write(true).open(&fifo);
        }
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(shown, Ok(0), "refused at once, never shown");
    }

    /// `o=z`: libghostty inflates the payload (wuffs since ghostty `d48c0372f`) before the
    /// engine sees the pixels.
    #[test]
    fn a_zlib_compressed_transmission_arrives_inflated() {
        let mut e = engine();
        let pixels: Vec<u8> = (0..16).collect();
        let transmit =
            format!("\x1b_Ga=T,f=32,o=z,s=2,v=2,i=3;{}\x1b\\", base64(&zlib_stored(&pixels)));
        e.write(transmit.as_bytes());
        let frame = e.take_frame(0).unwrap().expect("a frame");
        assert_eq!(frame.images.iter().map(|p| p.image).collect::<Vec<_>>(), vec![3]);
        let rgba: Vec<(u32, Vec<u8>)> =
            e.drain_images().into_iter().map(|u| (u.id, u.rgba)).collect();
        assert_eq!(rgba, vec![(3, pixels)]);
    }
}
