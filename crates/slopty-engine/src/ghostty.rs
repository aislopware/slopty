//! [`VtEngine`] backed by libghostty-vt.

use std::cell::RefCell;
use std::rc::Rc;

use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
use libghostty_vt::render::{CellIterator, Dirty, RenderState, RowIterator};
use libghostty_vt::screen::{Screen as VtScreen, TrackedGridRef};
use libghostty_vt::selection::Selection;
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, PointSpace};
use libghostty_vt::{Terminal, focus, key, mouse, paste};
use slopty_core::{Duration, MonoTime};
use slopty_grid::{
    Cell, CellText, Cursor, CursorShape, Line, LineFlags, LineIndex, RowUpdate, SemanticMark,
    Style, TermModes,
};
use slopty_proto::input::{KeyEvent, MouseAction, MouseEvent};
use slopty_proto::terminal::{Frame, TermSize};

use crate::{EngineConfig, EngineError, EngineEvent, VtEngine, convert, search};

/// How long a program may hold synchronized output (mode 2026) before we ship frames anyway.
const SYNC_OUTPUT_TIMEOUT: Duration = Duration::from_millis(1000);

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
    sync_since: Option<MonoTime>,
    buttons_down: u8,
    scratch: String,
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
            sync_since: None,
            buttons_down: 0,
            scratch: String::with_capacity(16),
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
        tracing::debug!(epoch = self.epoch, "line numbering invalidated");
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
        let mut updates = Vec::with_capacity(if full { usize::from(rows) } else { 8 });

        let mut row_iter = self.rows_iter.update(&snapshot)?;
        let mut y: u16 = 0;
        while let Some(row) = row_iter.next() {
            if full || row.dirty()? {
                let raw = row.raw_row()?;
                let mut line = Line::blank(cols);
                let mut first_semantic = None;
                let mut cell_iter = self.cells_iter.update(row)?;
                let mut x = 0_usize;
                while let Some(cell) = cell_iter.next() {
                    let Some(slot) = line.cells.get_mut(x) else { break };
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
                    *slot = Cell { text, style, width, hyperlink: None };
                    x = x.saturating_add(1);
                }
                line.flags.set(LineFlags::WRAPPED, raw.is_wrap_continuation()?);
                line.mark = first_semantic.map_or(SemanticMark::Unknown, |first| {
                    convert::semantic_mark(
                        raw.semantic_prompt()
                            .unwrap_or(libghostty_vt::screen::RowSemanticPrompt::None),
                        first,
                    )
                });
                updates.push(RowUpdate { row: y, line });
                row.set_dirty(false)?;
            }
            y = y.saturating_add(1);
        }
        snapshot.set_dirty(Dirty::Clean)?;

        let total = self.total_rows()?;
        let scrollback = self.term.scrollback_rows()? as u64;
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
        for x in 0..cols {
            let gr = self.term.grid_ref(Point::Screen(PointCoordinate { x, y: screen_y }))?;
            if row_info.is_none() {
                row_info = Some(gr.row()?);
            }
            let rc = gr.cell()?;
            if first_semantic.is_none() {
                first_semantic = Some(rc.semantic_content()?);
            }
            let width = convert::cell_width(rc.wide()?);
            let style =
                if rc.has_styling()? { convert::style(&gr.style()?) } else { Style::DEFAULT };
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
                            Cell {
                                text: CellText::from_cluster(&s),
                                style,
                                width,
                                hyperlink: None,
                            },
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
            set_cell(&mut line, x, Cell { text, style, width, hyperlink: None });
        }
        if let Some(row) = row_info {
            line.flags.set(LineFlags::WRAPPED, row.is_wrap_continuation()?);
            line.mark = first_semantic.map_or(SemanticMark::Unknown, |first| {
                convert::semantic_mark(
                    row.semantic_prompt().unwrap_or(libghostty_vt::screen::RowSemanticPrompt::None),
                    first,
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
        self.term.vt_write(bytes);
        if let Err(e) = self.settle() {
            tracing::error!(error = %e, "engine settle failed; invalidating line numbering");
            self.bump_epoch();
        }
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

    fn search(&self, needle: &str, max: u32) -> Result<search::Found, EngineError> {
        if needle.is_empty() {
            return Ok(search::Found::default());
        }
        let text = self.plain_text()?;
        Ok(search::find(&text, needle, LineIndex(self.base), max))
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
        let found = e.search("needle", 100).unwrap();
        assert_eq!(found.total, 1);
        let total = e.total_lines().unwrap();
        // The needle sits on the cursor row: the newest line.
        assert_eq!(found.matches[0].line, LineIndex(total - 1));
        assert_eq!((found.matches[0].col, found.matches[0].len), (0, 6));
        let found = e.search("line 7", 100).unwrap();
        // "line 7", "line 70".."line 79", "line 7x" not written beyond 199: 1 + 10 = 11.
        assert_eq!(found.total, 11);
        assert_eq!(found.matches[0].line, LineIndex(7));
        assert_eq!(e.search("", 10).unwrap(), search::Found::default());
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
