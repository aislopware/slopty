//! [`VtEngine`] backed by libghostty-vt.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
use libghostty_vt::render::{CellIterator, Dirty, RenderState, RowIterator};
use libghostty_vt::screen::{GridRef, Screen as VtScreen, TrackedGridRef};
use libghostty_vt::selection::Selection;
use libghostty_vt::terminal::{ClipboardLocation, Mode, Point, PointCoordinate, PointSpace};
use libghostty_vt::{Terminal, focus, key, mouse, paste};
use slopty_core::{Duration, MonoTime};
use slopty_grid::{
    Cell, CellText, CellWidth, Cursor, CursorShape, Hyperlink, Line, LineFlags, LineIndex,
    RowUpdate, SemanticMark, Style, TermModes,
};
use slopty_proto::input::{KeyEvent, MouseAction, MouseEvent};
use slopty_proto::terminal::{Frame, TermSize};

use crate::{EngineConfig, EngineError, EngineEvent, VtEngine, convert, osc133, search};

/// How long a program may hold synchronized output (mode 2026) before we ship frames anyway.
const SYNC_OUTPUT_TIMEOUT: Duration = Duration::from_millis(1000);

/// A prompt row takes the exit status of a `133;D` this many rows above it at most: the shell
/// may print a blank line or a partial-line marker between the mark and the prompt.
const EXIT_LOOKBACK_ROWS: u64 = 4;

type Events = Rc<RefCell<Vec<EngineEvent>>>;

/// libghostty-vt engine. `!Send`: lives on the session thread that owns the PTY reader.
pub struct GhosttyEngine {
    // Dropped before `term` (declaration order): it holds a pointer to the terminal.
    anchor: Option<Anchor>,
    primary_anchor: Option<Anchor>,
    term: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows_iter: RowIterator<'static>,
    cells_iter: CellIterator<'static>,
    key_enc: key::Encoder<'static>,
    key_ev: key::Event<'static>,
    mouse_enc: mouse::Encoder<'static>,
    mouse_ev: mouse::Event<'static>,
    events: Events,
    size: TermSize,
    seq: u64,
    epoch: u32,
    /// Absolute index of screen row 0 of the active screen.
    base: u64,
    on_alt: bool,
    /// The primary screen as VT bytes, taken just before the program switched to the alternate
    /// screen, so a checkpoint made on the alternate screen can carry both.
    primary_snapshot: Option<Vec<u8>>,
    /// The tail of the last chunk when it ended inside a possible alternate-screen switch
    /// (`ESC [ ? 10`), so a switch split across two reads is still seen before it completes.
    alt_prefix: Vec<u8>,
    sync_since: Option<MonoTime>,
    buttons_down: u8,
    scratch: String,
    /// Scratch for OSC 8 URIs (`ghostty_grid_ref_hyperlink_uri` wants a caller buffer).
    uri_buf: Vec<u8>,
    /// Watches the bytes for `OSC 133;A` and `133;D`, which libghostty does not surface.
    osc: osc133::Scanner,
    /// Exit status reported on an absolute line (the row the cursor was on at the `D`).
    exit_marks: BTreeMap<u64, Option<u8>>,
    /// Absolute lines a primary prompt started on (`133;A`).
    prompt_starts: BTreeSet<u64>,
}

/// A tracked row plus its absolute index.
struct Anchor {
    tracked: TrackedGridRef,
    abs: u64,
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

        let events: Events = Rc::new(RefCell::new(Vec::new()));
        install_callbacks(&mut term, &events)?;

        let mut engine = Self {
            anchor: None,
            primary_anchor: None,
            term,
            render: RenderState::new()?,
            rows_iter: RowIterator::new()?,
            cells_iter: CellIterator::new()?,
            key_enc: key::Encoder::new()?,
            key_ev: key::Event::new()?,
            mouse_enc: mouse::Encoder::new()?,
            mouse_ev: mouse::Event::new()?,
            events,
            size: config.size,
            seq: 0,
            epoch: 0,
            base: 0,
            on_alt: false,
            primary_snapshot: None,
            alt_prefix: Vec::new(),
            sync_since: None,
            buttons_down: 0,
            scratch: String::with_capacity(16),
            uri_buf: vec![0; 256],
            osc: osc133::Scanner::default(),
            exit_marks: BTreeMap::new(),
            prompt_starts: BTreeSet::new(),
        };
        engine.reanchor()?;
        Ok(engine)
    }

    /// Every retained row (history then screen) as plain text, one line per row with trailing
    /// blanks trimmed; blank rows at the very end are omitted. Measured at 0.4 ms for ~900
    /// rows of 80 columns (see `docs/MEASUREMENTS.md`).
    fn plain_text(&self) -> Result<String, EngineError> {
        let total = u32::try_from(self.total_rows()?).unwrap_or(u32::MAX);
        let last_col = self.size.cols.saturating_sub(1);
        let start = self.term.grid_ref(Point::Screen(PointCoordinate { x: 0, y: 0 }))?;
        let end = self
            .term
            .grid_ref(Point::Screen(PointCoordinate { x: last_col, y: total.saturating_sub(1) }))?;
        let selection = Selection::new(start, end, false);
        let options = FormatterOptions::new()
            .with_format(Format::Plain)
            .with_trim(true)
            .with_selection(&selection);
        let mut formatter = Formatter::new(&self.term, options)?;
        let bytes = formatter.format_alloc(None)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The whole terminal as the VT byte stream that rebuilds it in a fresh engine of the same
    /// size: palette, modes, scrolling region, working directory (OSC 7), keyboard
    /// state, every retained row (history then screen, soft wraps kept), and the cursor with its
    /// pending style and hyperlink. libghostty-vt's own formatter writes it, so what a program
    /// drew comes back exactly as its cells, not as an approximation from the grid.
    ///
    /// When the alternate screen is active the formatter can only see that screen, so the bytes
    /// are the primary screen as of the moment the program switched (kept by [`Self::write`])
    /// followed by the alternate screen; a program that leaves the alternate screen after the
    /// replay finds its primary where it was.
    ///
    /// # Errors
    ///
    /// When the formatter fails.
    pub fn checkpoint(&mut self) -> Result<Vec<u8>, EngineError> {
        let active = self.format_active_screen()?;
        if !self.on_alt {
            // Also the fallback for a switch [`Self::write`] fails to see: at worst the primary
            // comes back as of this checkpoint.
            self.primary_snapshot = Some(active.clone());
            return Ok(active);
        }
        let primary = self.primary_snapshot.clone().unwrap_or_default();
        let mut out = primary.clone();
        // Each blob only sets the modes that differ from the defaults, so a mode the primary
        // had on and the program turned off on the alternate screen would stay on: put every
        // mode the primary set back to its default before the alternate screen sets its own.
        out.extend_from_slice(&mode_resets(&primary));
        // Enter the alternate screen here, saving the primary cursor the snapshot just placed,
        // and home: the formatter writes content from wherever the cursor is, and it is where
        // the primary left it. Its own `?1049h` (in the modes it emits) is then a no-op.
        out.extend_from_slice(b"\x1b[?1049h\x1b[H");
        out.extend_from_slice(&active);
        Ok(out)
    }

    /// The active screen and the terminal state around it, as VT bytes.
    ///
    /// The formatter leaves two things to us. It drops trailing blank rows, so a primary screen
    /// that has scrolled would come back with too little history and its rows shifted up; the
    /// missing rows are replayed as line feeds, placed before any scrolling region so they
    /// scroll into history and not inside the region. And its cursor position comes before
    /// the scrolling region, which homes the cursor when set; the cursor is written last here
    /// instead, with origin mode lifted around it so the row is absolute.
    fn format_active_screen(&self) -> Result<Vec<u8>, EngineError> {
        let options = FormatterOptions::new()
            .with_format(Format::Vt)
            .with_unwrap(false)
            .with_trim(false)
            .with_palette(true)
            .with_modes(true)
            .with_scrolling_region(true)
            // Not tab stops: emitting them (`CSI 3 g`, then `CSI n G` + `ESC H` per stop) leaves
            // the cursor at the last stop and the content that follows starts there, shifted.
            // Programs do not set tab stops; the defaults every 8 columns are what a fresh
            // engine has anyway.
            .with_tabstops(false)
            .with_pwd(true)
            .with_keyboard(true)
            .with_cursor(false)
            .with_style(true)
            .with_hyperlink(true)
            .with_protection(true)
            .with_kitty_keyboard(true)
            .with_charsets(true);
        let mut formatter = Formatter::new(&self.term, options)?;
        let bytes = formatter.format_alloc(None)?;
        let mut out = Vec::with_capacity(bytes.len().saturating_add(64));
        if self.on_alt {
            // No history behind the alternate screen: nothing to scroll back into place.
            out.extend_from_slice(&bytes);
        } else {
            let (before, after) = bytes.split_at(margins_at(&bytes).map_or(bytes.len(), |m| m.at));
            out.extend_from_slice(before);
            // Rows are separated by CR LF and nothing else in the output contains one, so the
            // cursor stands on row `separators` of `total`; feed lines until it is on the last.
            let total = self.term.scrollback_rows()?.saturating_add(usize::from(self.term.rows()?));
            let separators = memchr::memmem::find_iter(&bytes, b"\r\n").count();
            for _ in 0..total.saturating_sub(1).saturating_sub(separators) {
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(after);
        }
        let (mut row, mut col) =
            (u32::from(self.term.cursor_y()?), u32::from(self.term.cursor_x()?));
        if self.term.mode(Mode::ORIGIN)? {
            // Setting or clearing origin mode homes the cursor, so it cannot be lifted around
            // the move: address the cursor the way the program does, relative to the margins.
            let margins = margins_at(&bytes).unwrap_or_default();
            row = row.saturating_sub(margins.top);
            col = col.saturating_sub(margins.left);
        }
        out.extend_from_slice(
            format!("\x1b[{};{}H", row.saturating_add(1), col.saturating_add(1)).as_bytes(),
        );
        Ok(out)
    }

    /// Current epoch of line numbering.
    #[must_use]
    pub const fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Absolute index of the oldest retrievable line.
    #[must_use]
    pub const fn oldest_line(&self) -> LineIndex {
        LineIndex(self.base)
    }

    /// Absolute index one past the newest line (history + screen).
    pub fn total_lines(&self) -> Result<u64, EngineError> {
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
        self.epoch = self.epoch.wrapping_add(1);
        self.base = 0;
        self.exit_marks.clear();
        self.prompt_starts.clear();
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
                self.primary_snapshot = self.format_active_screen().ok();
            }
        }
        let switch_at = if self.on_alt { None } else { alt_enter_at(chunk) };
        let rest = match switch_at {
            Some(at) => {
                let (before, from_switch) = chunk.split_at(at);
                self.term.vt_write(before);
                self.settle_or_bump();
                if !self.on_alt {
                    self.primary_snapshot = self.format_active_screen().ok();
                }
                from_switch
            }
            None => chunk,
        };
        self.term.vt_write(rest);
        self.settle_or_bump();
        if !self.on_alt {
            self.alt_prefix = alt_prefix_of(chunk).to_vec();
        }
    }

    fn settle_or_bump(&mut self) {
        if let Err(e) = self.settle() {
            tracing::error!(error = %e, "engine settle failed; invalidating line numbering");
            self.bump_epoch();
        }
    }

    /// The shell wrote a prompt mark at the cursor: remember which line, and the status.
    fn record_mark(&mut self, mark: osc133::Mark) {
        let Ok(y) = self.term.cursor_y() else { return };
        let Ok(scrollback) = self.term.scrollback_rows() else { return };
        let line = self.base.saturating_add(scrollback as u64).saturating_add(u64::from(y));
        // Evicted history can never be read again; drop its marks with it.
        self.exit_marks = self.exit_marks.split_off(&self.base);
        self.prompt_starts = self.prompt_starts.split_off(&self.base);
        match mark {
            osc133::Mark::PromptStart => {
                self.prompt_starts.insert(line);
            }
            osc133::Mark::CommandEnd { exit } => {
                self.exit_marks.insert(line, exit);
            }
        }
    }

    /// After output was consumed: follow the anchor to keep `base` exact, handle screen switches.
    fn settle(&mut self) -> Result<(), EngineError> {
        let on_alt = self.term.active_screen()? == VtScreen::Alternate;
        if on_alt != self.on_alt {
            self.on_alt = on_alt;
            if on_alt {
                // Park the primary anchor; alt screen has no history and starts at 0.
                self.primary_anchor = self.anchor.take();
                self.bump_epoch();
                self.reanchor()?;
                return Ok(());
            }
            // Back on primary: restore numbering from the parked anchor if it survived.
            self.anchor = self.primary_anchor.take();
            self.epoch = self.epoch.wrapping_add(1);
        }

        let followed = match &self.anchor {
            Some(anchor) => anchor
                .tracked
                .point(PointSpace::Screen)?
                .map(|p| anchor.abs.saturating_sub(u64::from(p.y))),
            None => None,
        };
        match followed {
            Some(base) => self.base = base,
            None => self.bump_epoch(),
        }
        self.reanchor()
    }

    fn cursor(snapshot: &libghostty_vt::render::Snapshot<'_, '_>) -> Result<Cursor, EngineError> {
        let (row, col) = snapshot.cursor_viewport()?.map_or((0, 0), |c| (c.y, c.x));
        Ok(Cursor {
            row,
            col,
            shape: snapshot.cursor_visual_style().map_or(CursorShape::Block, convert::cursor_shape),
            visible: snapshot.cursor_visible()?,
            blink: snapshot.cursor_blinking()?,
        })
    }

    fn modes_inner(&self) -> Result<TermModes, EngineError> {
        let t = &self.term;
        let mut m = TermModes::empty();
        m.set(TermModes::ALT_SCREEN, self.on_alt);
        m.set(TermModes::MOUSE_TRACKING, t.is_mouse_tracking()?);
        m.set(TermModes::ALT_SCROLL, t.mode(Mode::ALT_SCROLL)?);
        m.set(TermModes::BRACKETED_PASTE, t.mode(Mode::BRACKETED_PASTE)?);
        m.set(TermModes::FOCUS_EVENTS, t.mode(Mode::FOCUS_EVENT)?);
        m.set(TermModes::KITTY_KEYBOARD, !t.kitty_keyboard_flags()?.is_empty());
        m.set(TermModes::SYNC_OUTPUT, t.mode(Mode::SYNC_OUTPUT)?);
        m.set(TermModes::CURSOR_HIDDEN, !t.mode(Mode::CURSOR_VISIBLE)?);
        m.set(TermModes::APP_CURSOR_KEYS, t.mode(Mode::DECCKM)?);
        Ok(m)
    }

    /// Whether synchronized output is holding frames back (with a timeout so a stuck program
    /// cannot freeze the display).
    fn sync_held(&mut self) -> Result<bool, EngineError> {
        if !self.term.mode(Mode::SYNC_OUTPUT)? {
            self.sync_since = None;
            return Ok(false);
        }
        let since = *self.sync_since.get_or_insert_with(MonoTime::now);
        Ok(since.elapsed() < SYNC_OUTPUT_TIMEOUT)
    }

    fn build_frame(
        &mut self,
        input_ack: u64,
        force_full: bool,
    ) -> Result<Option<Frame>, EngineError> {
        let snapshot = self.render.update(&self.term)?;
        let dirty = snapshot.dirty()?;
        let full = force_full || dirty == Dirty::Full;
        if !full && dirty == Dirty::Clean {
            return Ok(None);
        }

        let cols = snapshot.cols()?;
        let rows = snapshot.rows()?;
        let cursor = Self::cursor(&snapshot)?;
        let scrollback = self.term.scrollback_rows()? as u64;
        let mut updates = Vec::with_capacity(if full { usize::from(rows) } else { 8 });

        let mut row_iter = self.rows_iter.update(&snapshot)?;
        let mut y: u16 = 0;
        while let Some(row) = row_iter.next() {
            if full || row.dirty()? {
                let raw = row.raw_row()?;
                let mut line = Line::blank(cols);
                let mut first_semantic = None;
                // The row flag may be a false positive, but a row without it has no links.
                let row_has_links = raw.has_hyperlink()?;
                let mut links = LinkRuns::default();
                let mut cell_iter = self.cells_iter.update(row)?;
                let mut x: u16 = 0;
                while let Some(cell) = cell_iter.next() {
                    let Some(slot) = line.cells.get_mut(usize::from(x)) else { break };
                    let rc = cell.raw_cell()?;
                    if first_semantic.is_none() {
                        first_semantic = Some(rc.semantic_content()?);
                    }
                    let style = if cell.has_styling()? {
                        convert::style(&cell.style()?)
                    } else {
                        Style::DEFAULT
                    };
                    let width = convert::cell_width(rc.wide()?);
                    let text = if rc.has_text()? && width.draws_text() {
                        self.scratch.clear();
                        cell.graphemes_utf8(&mut self.scratch)?;
                        CellText::from_cluster(&self.scratch)
                    } else {
                        CellText::EMPTY
                    };
                    if row_has_links {
                        let uri = if rc.has_hyperlink()? {
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
                line.flags.set(LineFlags::WRAPPED, raw.is_wrap_continuation()?);
                let abs = self.base.saturating_add(scrollback).saturating_add(u64::from(y));
                line.mark = first_semantic.map_or(SemanticMark::Unknown, |first| {
                    convert::semantic_mark(
                        raw.semantic_prompt()
                            .unwrap_or(libghostty_vt::screen::RowSemanticPrompt::None),
                        first,
                        self.prompt_starts.contains(&abs),
                        exit_for(&self.exit_marks, &self.prompt_starts, abs),
                    )
                });
                updates.push(RowUpdate { row: y, line });
                row.set_dirty(false)?;
            }
            y = y.saturating_add(1);
        }
        snapshot.set_dirty(Dirty::Clean)?;

        let total = self.total_rows()?;
        self.seq = self.seq.wrapping_add(1);
        Ok(Some(Frame {
            seq: self.seq,
            full,
            epoch: self.epoch,
            cols,
            rows,
            cursor,
            modes: self.modes_inner()?,
            oldest_line: LineIndex(self.base),
            first_visible_line: LineIndex(self.base.saturating_add(scrollback)),
            total_lines: self.base.saturating_add(total),
            input_ack,
            updates,
        }))
    }

    fn read_line(&self, screen_y: u32, cols: u16) -> Result<Line, EngineError> {
        let mut line = Line::blank(cols);
        let mut chars = [char::MIN; 16];
        let mut first_semantic = None;
        let mut row_info = None;
        let mut row_has_links = false;
        let mut links = LinkRuns::default();
        let mut uri_buf = vec![0; 256];
        for x in 0..cols {
            let gr = self.term.grid_ref(Point::Screen(PointCoordinate { x, y: screen_y }))?;
            if row_info.is_none() {
                let row = gr.row()?;
                row_has_links = row.has_hyperlink()?;
                row_info = Some(row);
            }
            let rc = gr.cell()?;
            if first_semantic.is_none() {
                first_semantic = Some(rc.semantic_content()?);
            }
            let width = convert::cell_width(rc.wide()?);
            let style =
                if rc.has_styling()? { convert::style(&gr.style()?) } else { Style::DEFAULT };
            if row_has_links {
                let uri =
                    if rc.has_hyperlink()? { hyperlink_uri(&gr, &mut uri_buf)? } else { None };
                links.push(x, uri, width == CellWidth::SpacerTail);
            }
            let text = if rc.has_text()? && width.draws_text() {
                let n = match gr.graphemes(&mut chars) {
                    Ok(n) => n,
                    Err(libghostty_vt::Error::OutOfSpace { required }) => {
                        let mut big = vec![char::MIN; required];
                        let n = gr.graphemes(&mut big)?;
                        let s: String = big.iter().take(n).collect();
                        set_cell(
                            &mut line,
                            x,
                            Cell { text: CellText::from_cluster(&s), style, width },
                        );
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                let s: String = chars.iter().take(n).collect();
                CellText::from_cluster(&s)
            } else {
                CellText::EMPTY
            };
            set_cell(&mut line, x, Cell { text, style, width });
        }
        line.links = links.finish(cols);
        if let Some(row) = row_info {
            line.flags.set(LineFlags::WRAPPED, row.is_wrap_continuation()?);
            let abs = self.base.saturating_add(u64::from(screen_y));
            line.mark = first_semantic.map_or(SemanticMark::Unknown, |first| {
                convert::semantic_mark(
                    row.semantic_prompt().unwrap_or(libghostty_vt::screen::RowSemanticPrompt::None),
                    first,
                    self.prompt_starts.contains(&abs),
                    exit_for(&self.exit_marks, &self.prompt_starts, abs),
                )
            });
        }
        Ok(line)
    }

    /// Whether the terminal's viewport is showing the active area. The host never scrolls it, but
    /// a program can't either, so this is a debug assertion helper.
    #[must_use]
    pub fn viewport_pinned(&self) -> bool {
        self.term.viewport_active().unwrap_or(true)
    }
}

/// The margins the formatter wrote, if any, and where its first margin sequence starts.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Margins {
    /// Byte offset of the first of `DECSTBM` / `DECSLRM`.
    at: usize,
    /// Top margin, 0-based.
    top: u32,
    /// Left margin, 0-based.
    left: u32,
}

/// The formatter writes `DECSTBM` (`CSI top ; bottom r`) and `DECSLRM` (`CSI left ; right s`)
/// independently, each only when set. Cells never hold an escape and nothing else it writes
/// ends in `r` or in `s` with parameters, so the first match of each is it.
fn margins_at(bytes: &[u8]) -> Option<Margins> {
    let stbm = csi_with_final(bytes, b'r');
    let slrm = csi_with_final(bytes, b's');
    let at = match (stbm, slrm) {
        (Some((a, _)), Some((b, _))) => a.min(b),
        (Some((a, _)), None) | (None, Some((a, _))) => a,
        (None, None) => return None,
    };
    Some(Margins {
        at,
        top: stbm.map_or(0, |(_, first)| first.saturating_sub(1)),
        left: slrm.map_or(0, |(_, first)| first.saturating_sub(1)),
    })
}

/// The first `CSI params final` in `bytes` whose parameters are digits and `;` only (at least
/// one digit) and whose final byte is `last`: its offset and its first parameter.
fn csi_with_final(bytes: &[u8], last: u8) -> Option<(usize, u32)> {
    memchr::memmem::find_iter(bytes, b"\x1b[").find_map(|at| {
        let rest = bytes.get(at.saturating_add(2)..).unwrap_or_default();
        let params = rest.iter().take_while(|b| b.is_ascii_digit() || **b == b';').count();
        if params == 0 || rest.get(params) != Some(&last) {
            return None;
        }
        let first = rest.get(..params)?.split(|b| *b == b';').next()?;
        let first: u32 = std::str::from_utf8(first).ok()?.parse().ok()?;
        Some((at, first))
    })
}

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
    if !(host.is_empty() || host == "localhost" || host.eq_ignore_ascii_case(&hostname())) {
        return None;
    }
    if path.is_empty() {
        return None;
    }
    Some(percent_decode(path))
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
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

fn set_cell(line: &mut Line, x: u16, cell: Cell) {
    if let Some(slot) = line.cells.get_mut(usize::from(x)) {
        *slot = cell;
    }
}

const fn check_size(size: TermSize) -> Result<(), EngineError> {
    if size.cols == 0 || size.rows == 0 {
        return Err(EngineError::InvalidSize("zero columns or rows"));
    }
    if size.metrics.cell_width == 0 || size.metrics.cell_height == 0 {
        return Err(EngineError::InvalidSize("zero cell metrics"));
    }
    Ok(())
}

fn install_callbacks(
    term: &mut Terminal<'static, 'static>,
    events: &Events,
) -> Result<(), EngineError> {
    let for_pty = Rc::clone(events);
    term.on_pty_write(move |_, data: &[u8]| {
        for_pty.borrow_mut().push(EngineEvent::PtyWrite(data.to_vec()));
    })?;
    let for_bell = Rc::clone(events);
    term.on_bell(move |_| for_bell.borrow_mut().push(EngineEvent::Bell))?;
    let for_title = Rc::clone(events);
    term.on_title_changed(move |t| {
        let title = t.title().unwrap_or_default().to_owned();
        for_title.borrow_mut().push(EngineEvent::Title(title));
    })?;
    let for_pwd = Rc::clone(events);
    term.on_pwd_changed(move |t| {
        if let Some(path) = cwd_from_osc7(t.pwd().unwrap_or_default()) {
            for_pwd.borrow_mut().push(EngineEvent::Cwd(path));
        }
    })?;
    let for_clip = Rc::clone(events);
    term.on_clipboard_write(move |_, write| {
        // Only the system clipboard; selection/primary are X11 notions with no counterpart
        // on the clients. Reads (OSC 52 `?`) never reach this callback (libghostty drops
        // them), and Slopty does not answer them anywhere else.
        if write.location() != ClipboardLocation::Standard {
            return Err(libghostty_vt::terminal::ClipboardWriteError::Unsupported);
        }
        let text = write
            .contents()
            .find(|c| c.mime.starts_with("text/plain"))
            .map(|c| String::from_utf8_lossy(c.data).into_owned());
        match text {
            Some(text) => {
                for_clip.borrow_mut().push(EngineEvent::ClipboardWrite { text });
                Ok(())
            }
            None => Err(libghostty_vt::terminal::ClipboardWriteError::Unsupported),
        }
    })?;
    Ok(())
}

impl VtEngine for GhosttyEngine {
    fn write(&mut self, bytes: &[u8]) {
        // Feed up to each prompt mark separately so the cursor row at the mark is exact.
        let mut rest = bytes;
        while let Some(found) = self.osc.scan(rest) {
            let (head, tail) = rest.split_at(found.end.min(rest.len()));
            self.feed(head);
            self.record_mark(found.mark);
            rest = tail;
        }
        self.feed(rest);
    }

    fn resize(&mut self, size: TermSize) -> Result<(), EngineError> {
        check_size(size)?;
        if size == self.size {
            return Ok(());
        }
        let reflow = size.cols != self.size.cols || size.rows != self.size.rows;
        self.term.resize(
            size.cols,
            size.rows,
            u32::from(size.metrics.cell_width),
            u32::from(size.metrics.cell_height),
        )?;
        self.size = size;
        if reflow {
            self.primary_anchor = None;
            self.bump_epoch();
            self.reanchor()?;
        }
        Ok(())
    }

    fn size(&self) -> TermSize {
        self.size
    }

    fn take_frame(&mut self, input_ack: u64) -> Result<Option<Frame>, EngineError> {
        if self.sync_held()? {
            return Ok(None);
        }
        self.build_frame(input_ack, false)
    }

    fn full_frame(&mut self, input_ack: u64) -> Result<Frame, EngineError> {
        self.build_frame(input_ack, true)?.ok_or(EngineError::InvalidSize("empty frame"))
    }

    fn lines(&self, start: LineIndex, count: u32) -> Result<(LineIndex, Vec<Line>), EngineError> {
        let total = self.total_lines()?;
        let first = start.0.max(self.base);
        let end = first.saturating_add(u64::from(count)).min(total);
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

    fn modes(&self) -> Result<TermModes, EngineError> {
        self.modes_inner()
    }

    fn search(&self, needle: &str, regex: bool, max: u32) -> Result<search::Found, EngineError> {
        if needle.is_empty() {
            return Ok(search::Found::default());
        }
        let pattern = search::Pattern::new(needle, regex).map_err(EngineError::Pattern)?;
        let text = self.plain_text()?;
        Ok(search::find(&text, &pattern, LineIndex(self.base), max))
    }

    fn encode_key(&mut self, event: &KeyEvent, out: &mut Vec<u8>) -> Result<(), EngineError> {
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
        self.key_enc.set_options_from_terminal(&self.term);
        match self.key_enc.encode_to_vec(ev, out) {
            // Not every key produces bytes; the encoder reports that as an invalid value.
            Ok(()) | Err(libghostty_vt::Error::InvalidValue) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn encode_mouse(&mut self, event: &MouseEvent, out: &mut Vec<u8>) -> Result<(), EngineError> {
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
                self.buttons_down = if press {
                    self.buttons_down.saturating_add(1)
                } else {
                    self.buttons_down.saturating_sub(1)
                };
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

    fn encode_paste(&mut self, text: &str, out: &mut Vec<u8>) -> Result<(), EngineError> {
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

    fn encode_focus(&mut self, focused: bool, out: &mut Vec<u8>) -> Result<(), EngineError> {
        if !self.term.mode(Mode::FOCUS_EVENT)? {
            return Ok(());
        }
        let ev = if focused { focus::Event::Gained } else { focus::Event::Lost };
        let mut buf = [0_u8; 8];
        let n = ev.encode(&mut buf)?;
        out.extend_from_slice(buf.get(..n).unwrap_or_default());
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<EngineEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use slopty_grid::{CellWidth, StyleFlags};
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
        assert_eq!((b.cols, b.rows), (20, 4));
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
        assert!(out.is_empty());
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
                button: Some(slopty_proto::input::MouseButton::Left),
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

    #[test]
    fn query_responses_and_osc_side_effects_are_events() {
        let mut e = engine(10, 3);
        e.write(b"\x1b[c\x07\x1b]0;hello\x07\x1b]7;file:///tmp\x07");
        let ev = e.drain_events();
        assert!(matches!(&ev[0], EngineEvent::PtyWrite(b) if b.starts_with(b"\x1b[?")));
        assert!(ev.contains(&EngineEvent::Bell));
        assert!(ev.contains(&EngineEvent::Title("hello".to_owned())));
        assert!(ev.contains(&EngineEvent::Cwd("/tmp".to_owned())));
        assert!(e.drain_events().is_empty());
    }

    #[test]
    fn osc7_urls_become_local_paths() {
        assert_eq!(cwd_from_osc7("file:///tmp"), Some("/tmp".to_owned()));
        assert_eq!(cwd_from_osc7("file://localhost/a%20b/c"), Some("/a b/c".to_owned()));
        assert_eq!(cwd_from_osc7("file://elsewhere/tmp"), None);
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
        assert_eq!(marks[0], SemanticMark::Prompt { exit: None }, "nothing ran before it");
        assert_eq!(marks[1], SemanticMark::Prompt { exit: Some(1) }, "adjacent prompts stay apart");
        assert_eq!(marks[2], SemanticMark::Output);
        assert_eq!(marks[3], SemanticMark::Output, "blank line the shell printed");
        assert_eq!(marks[4], SemanticMark::Prompt { exit: Some(0) }, "status survives the gap");
        assert_eq!(marks[5], SemanticMark::Output, "never written to");
        assert!(f.updates[1].line.text().starts_with("$ "));
        // A two-row prompt right under the last one (no command ran, so no status), scrolling
        // the first row into history: the second row belongs to the block above it.
        e.write(b"\r\n\x1b]133;A\x07~\r\n> \x1b]133;B\x07");
        let f = e.full_frame(0).unwrap();
        let marks: Vec<SemanticMark> = f.updates.iter().map(|u| u.line.mark).collect();
        assert_eq!(marks[3], SemanticMark::Prompt { exit: Some(0) });
        assert_eq!(marks[4], SemanticMark::Prompt { exit: None }, "the status above is taken");
        assert_eq!(marks[5], SemanticMark::PromptContinuation);
        // The same blocks on the history path.
        e.write(b"\r\n\r\n\r\n");
        let (_, lines) = e.lines(LineIndex(0), 7).unwrap();
        let marks: Vec<SemanticMark> = lines.iter().map(|l| l.mark).collect();
        assert_eq!(marks[1], SemanticMark::Prompt { exit: Some(1) });
        assert_eq!(marks[4], SemanticMark::Prompt { exit: Some(0) });
        assert_eq!(marks[5], SemanticMark::Prompt { exit: None });
        assert_eq!(marks[6], SemanticMark::PromptContinuation);
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
        let prompt = |exit| SemanticMark::Prompt { exit };
        assert_eq!(rows[0], (String::new(), prompt(None)));
        assert_eq!(rows[1], ("/tmp ".to_owned(), SemanticMark::PromptContinuation));
        assert_eq!(rows[2].0, "> seq 1 3");
        assert_eq!(rows[3], ("1".to_owned(), SemanticMark::Output));
        assert_eq!(rows[6], (String::new(), prompt(Some(0))), "status of seq, D then A");
        assert_eq!(rows[8].0, "> false");
        assert_eq!(rows[9], (String::new(), prompt(Some(1))), "status of false");
        assert_eq!(rows[10], ("/tmp exit 1 ".to_owned(), SemanticMark::PromptContinuation));
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
        // A read request ("?") produces neither an event nor a reply to the program.
        e.write(b"\x1b]52;c;?\x07");
        assert_eq!(e.drain_events(), vec![]);
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
        let (_start, lines) = e.lines(e.oldest_line(), total).unwrap();
        lines.iter().map(|l| l.text().trim_end().to_owned()).collect()
    }

    #[test]
    fn a_checkpoint_rebuilds_history_screen_cursor_and_modes() {
        let mut a = engine(12, 3, 100);
        for i in 0..5 {
            a.write(format!("line {i}\r\n").as_bytes());
        }
        a.write(b"\x1b[1;31mred\x1b[0m plain\x1b[?2004h\x1b[?1h\x1b[2;4H");
        let checkpoint = a.checkpoint().unwrap();

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
        let checkpoint = a.checkpoint().unwrap();

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
        let checkpoint = a.checkpoint().unwrap();
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
        let checkpoint = a.checkpoint().unwrap();
        assert!(margins_at(&checkpoint).is_some());
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

    /// `cargo nextest run -p slopty-engine --release --run-ignored only checkpoint_cost
    /// --no-capture`: how long a checkpoint takes and how big it is at 80x24 with 10 000 lines
    /// of history, the number behind the checkpoint policy in `slopty_host::session` (recorded
    /// in MEASUREMENTS).
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
        let start = std::time::Instant::now();
        for chunk in out.chunks(65_536) {
            e.write(chunk);
        }
        let fill = start.elapsed();
        let mut raw = Terminal::new(80, 24).unwrap();
        raw.set_scrollback_max_lines(Some(10_000)).unwrap();
        let start = std::time::Instant::now();
        for chunk in out.chunks(65_536) {
            raw.vt_write(chunk);
        }
        let raw_fill = start.elapsed();
        eprintln!("checkpoint_cost: raw libghostty-vt fill {raw_fill:?} vs engine fill {fill:?}");
        let start = std::time::Instant::now();
        let bytes = e.checkpoint().unwrap();
        let took = start.elapsed();
        let start = std::time::Instant::now();
        let mut b = engine(80, 24, 10_000);
        b.write(&bytes);
        let replay = start.elapsed();
        let start = std::time::Instant::now();
        let mut c = engine(80, 24, 10_000);
        for chunk in bytes.chunks(65_536) {
            c.write(chunk);
        }
        let replay_chunked = start.elapsed();
        assert_eq!(all_text(&b), all_text(&e));
        assert_eq!(all_text(&c), all_text(&e));
        eprintln!(
            "checkpoint_cost: {} history lines; fill {} bytes in 64 KiB chunks {fill:?}; checkpoint {} bytes, format {took:?}, replay one chunk {replay:?}, replay 64 KiB chunks {replay_chunked:?}",
            e.total_lines().unwrap() - 24,
            out.len(),
            bytes.len()
        );
    }

    #[test]
    fn margins_are_read_from_the_formatter_sequences() {
        let bytes = b"abc\x1b[3;10r\x1b[2;7s\x1b]7;file:///\x1b\\";
        assert_eq!(margins_at(bytes), Some(Margins { at: 3, top: 2, left: 1 }));
        assert_eq!(margins_at(b"\x1b[3;10r"), Some(Margins { at: 0, top: 2, left: 0 }));
        // DECSLRM alone (full-height margins, a left margin): still found, padding before it.
        assert_eq!(margins_at(b"x\x1b[0m\x1b[5;20s"), Some(Margins { at: 5, top: 0, left: 4 }));
        assert!(margins_at(b"\x1b]4;0;rgb:00/00/00\x1b\\\x1b[?1049h\x1b[s").is_none());
    }

    #[test]
    fn mode_resets_invert_every_mode_but_the_screen_switch() {
        let blob =
            b"\x1b]4;0;rgb:00/00/00\x1b\\\x1b[?1h\x1b[4h\x1b[?7l\x1b[?1049h\x1b[?25l\x1b[2;1H";
        assert_eq!(mode_resets(blob), b"\x1b[?1l\x1b[4l\x1b[?7h\x1b[?25h");
        assert!(mode_resets(b"plain\x1b[31m\x1b[3;4r").is_empty());
    }

    #[test]
    fn an_alternate_screen_switch_split_across_reads_still_snapshots_the_primary() {
        let mut a = engine(20, 3, 100);
        a.write(b"before\r\n");
        a.write(b"\x1b[?10");
        assert!(!a.modes().unwrap().contains(TermModes::ALT_SCREEN));
        a.write(b"49h\x1b[Halt");
        assert!(a.modes().unwrap().contains(TermModes::ALT_SCREEN));
        let checkpoint = a.checkpoint().unwrap();
        let mut b = engine(20, 3, 100);
        b.write(&checkpoint);
        assert_eq!(all_text(&b), all_text(&a));
        a.write(b"\x1b[?1049l");
        b.write(b"\x1b[?1049l");
        assert_eq!(all_text(&b), ["before", "", ""]);
        assert_eq!(all_text(&b), all_text(&a));
    }

    #[test]
    fn a_mode_turned_off_on_the_alternate_screen_stays_off_after_replay() {
        let mut a = engine(20, 3, 100);
        a.write(b"\x1b[?1h\x1b[?1049h\x1b[?1l");
        assert!(!a.modes().unwrap().contains(TermModes::APP_CURSOR_KEYS));
        let checkpoint = a.checkpoint().unwrap();
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
        let _first = a.checkpoint().unwrap();
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
