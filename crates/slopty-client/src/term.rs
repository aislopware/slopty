//! Terminal session state on the client.

use std::collections::BTreeMap;
use std::sync::Arc;

use slopty_grid::{
    CellWidth, Cursor, Line, LineFlags, LineIndex, Screen, Scrollback, SemanticMark, TermModes,
};
use slopty_proto::terminal::{
    ColorOverrides, Frame, IMAGE_CACHE_BYTES, Placement, SearchMatch, TermEvent, TermRequest,
    TermSize,
};

/// Lines kept client-side. Newest-first eviction; the worker retains 50k.
pub const CACHE_LINES: usize = 20_000;

/// What the UI or connection should do after an event was applied.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Effect {
    /// Send this to the worker.
    Request(TermRequest),
    /// Ring the bell.
    Bell,
    /// The title changed.
    Title(String),
    /// The cwd changed, with the repository the worker resolved it to.
    Cwd {
        /// The new directory.
        path: String,
        /// Its repository root, if it is in one.
        repo: Option<String>,
    },
    /// The program wrote to the clipboard.
    ClipboardWrite(String),
    /// The program asked for a desktop notification.
    Notification {
        /// Its title, possibly empty.
        title: String,
        /// Its body.
        body: String,
    },
    /// The child exited.
    Exited(i32),
    /// The worker reported an error for a request.
    Error(String),
    /// Search hits for `needle`.
    Matches {
        /// The needle they answer.
        needle: String,
        /// Every hit, listed or not.
        total: u32,
        /// The newest hits, oldest first.
        matches: Vec<SearchMatch>,
    },
    /// The regex in a search did not compile.
    SearchInvalid {
        /// The needle it answers.
        needle: String,
        /// Why.
        message: String,
    },
    /// A shell command left its prompt: it is running (shell integration marks only).
    CommandStarted(String),
    /// The shell printed its next prompt: the running command finished.
    CommandFinished {
        /// The row the command was typed at (its block's prompt); `None` when the numbering
        /// changed while it ran and its prompt is not among the rows held here.
        prompt: Option<LineIndex>,
        /// What was typed.
        command: String,
        /// Its exit status, when the shell said.
        exit: Option<u8>,
    },
}

/// One row as the UI should draw it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ViewRow<'a> {
    /// Absolute index (for selection anchoring).
    pub index: LineIndex,
    /// The line, or `None` while it is being fetched.
    pub line: Option<&'a Line>,
}

/// The state of one attached terminal session.
#[derive(Clone, Debug)]
pub struct TermState {
    size: TermSize,
    screen: Screen,
    scrollback: Scrollback,
    epoch: Option<u32>,
    last_seq: Option<u64>,
    first_visible: LineIndex,
    input_ack: u64,
    /// Lines scrolled up from the bottom; 0 follows output.
    view_offset: u64,
    title: Option<String>,
    cwd: Option<String>,
    repo: Option<String>,
    exited: Option<i32>,
    driving: bool,
    resync_pending: bool,
    frames: u64,
    /// The newest prompt start seen this epoch; a newer one ends the running command.
    latest_prompt: Option<LineIndex>,
    /// The command running since the cursor left its prompt, with the prompt it was typed at.
    running: Option<(LineIndex, String)>,
    /// Images the worker sent, by id, for the placements of the frames.
    images: BTreeMap<u32, TermImage>,
    /// Bytes of pixels in `images`.
    image_bytes: usize,
    /// The placements of the latest frame, in paint order.
    placements: Vec<Placement>,
    /// The program's colour changes over the theme.
    colors: ColorOverrides,
    /// The primary screen's lines while the alternate screen is up, to take back if the
    /// worker returns to the same numbering.
    parked: Option<Parked>,
    /// Frames dropped as older than the last one applied: the tail of a stream a re-attach
    /// replaced.
    superseded: u64,
}

/// What the client held of the primary screen's numbering when a program took the alternate
/// screen.
#[derive(Clone, Debug)]
struct Parked {
    epoch: u32,
    scrollback: Scrollback,
    latest_prompt: Option<LineIndex>,
}

/// The pixels of one image the worker sent (kitty graphics).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TermImage {
    /// Its generation: a placement names the one it was laid out for.
    pub generation: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// RGBA, row-major; shared with whatever texture the view makes of it.
    pub rgba: Arc<[u8]>,
    /// The frame that last placed it (the cache drops the least recently placed first).
    pub last: u64,
}

/// The head of a command block, as [`TermState::block_head`] reads it from the marks.
///
/// The prompt and what was typed at it, without the output: what a per-frame reader (the
/// sticky header) needs, at the cost of the prompt's rows alone.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BlockHead {
    /// The row the prompt starts on.
    pub prompt: LineIndex,
    /// The status of the command that ran before this prompt, when the shell said.
    pub exit: Option<u8>,
    /// What was typed at the prompt, rows joined with newlines; `None` when nothing was.
    pub command: Option<String>,
    /// The first row after the prompt's and the command's rows: where the output starts.
    pub body: LineIndex,
}

/// One shell command block, as [`TermState::command_block`] reads it from the marks.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CommandBlock {
    /// The row the prompt starts on.
    pub prompt: LineIndex,
    /// The first row after the block (the next prompt's start, or one past the newest line).
    pub end: LineIndex,
    /// The status of the command that ran before this prompt, when the shell said.
    pub exit: Option<u8>,
    /// What was typed at the prompt, rows joined with newlines; `None` when nothing was.
    pub command: Option<String>,
    /// The output rows, trailing blank rows trimmed, joined with newlines.
    pub output: String,
}

impl TermState {
    /// Fresh state for a session of `size`.
    #[must_use]
    pub fn new(size: TermSize) -> Self {
        Self {
            size,
            screen: Screen::new(size.cols, size.rows),
            scrollback: Scrollback::new(CACHE_LINES),
            epoch: None,
            last_seq: None,
            first_visible: LineIndex(0),
            input_ack: 0,
            view_offset: 0,
            title: None,
            cwd: None,
            repo: None,
            exited: None,
            driving: false,
            resync_pending: false,
            frames: 0,
            latest_prompt: None,
            running: None,
            images: BTreeMap::new(),
            image_bytes: 0,
            placements: Vec::new(),
            colors: ColorOverrides::default(),
            parked: None,
            superseded: 0,
        }
    }

    /// The placements of the latest frame, in paint order.
    #[must_use]
    pub fn placements(&self) -> &[Placement] {
        &self.placements
    }

    /// The program's colour changes over the theme (OSC 4/10/11/12).
    #[must_use]
    pub const fn colors(&self) -> &ColorOverrides {
        &self.colors
    }

    /// The pixels a placement names, once the worker sent that generation.
    #[must_use]
    pub fn image(&self, placement: &Placement) -> Option<&TermImage> {
        self.images.get(&placement.image).filter(|i| i.generation == placement.generation)
    }

    /// Whether the pixels of `id` at `generation` are held.
    #[must_use]
    pub fn holds(&self, id: u32, generation: u64) -> bool {
        self.images.get(&id).is_some_and(|i| i.generation == generation)
    }

    /// Keep an image the worker sent.
    ///
    /// Drops the least recently placed ones over the budget the worker assumes
    /// (`IMAGE_CACHE_BYTES`), so both sides forget the same images.
    fn keep_image(&mut self, id: u32, image: TermImage) {
        if let Some(old) = self.images.insert(id, image) {
            self.image_bytes = self.image_bytes.saturating_sub(old.rgba.len());
        }
        if let Some(new) = self.images.get(&id) {
            self.image_bytes = self.image_bytes.saturating_add(new.rgba.len());
        }
        self.prune_images();
    }

    fn prune_images(&mut self) {
        while self.image_bytes > IMAGE_CACHE_BYTES {
            let Some((&id, _)) = self.images.iter().min_by_key(|(_, i)| i.last) else { break };
            if let Some(gone) = self.images.remove(&id) {
                self.image_bytes = self.image_bytes.saturating_sub(gone.rgba.len());
            }
        }
    }

    /// Current size.
    #[must_use]
    pub const fn size(&self) -> TermSize {
        self.size
    }

    /// The scrollback cache: what is held, and how much of it the worker still has.
    #[must_use]
    pub const fn scrollback(&self) -> &Scrollback {
        &self.scrollback
    }

    /// The live screen (bottom of the viewport).
    #[must_use]
    pub const fn screen(&self) -> &Screen {
        &self.screen
    }

    /// Cursor.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.screen.cursor()
    }

    /// Modes.
    #[must_use]
    pub const fn modes(&self) -> TermModes {
        self.screen.modes()
    }

    /// Title from OSC 0/2.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Cwd from OSC 7.
    #[must_use]
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// The repository the cwd is in, as the worker resolved it.
    #[must_use]
    pub fn repo(&self) -> Option<&str> {
        self.repo.as_deref()
    }

    /// Exit status once the child is gone.
    #[must_use]
    pub const fn exited(&self) -> Option<i32> {
        self.exited
    }

    /// Whether this client drives the PTY size.
    #[must_use]
    pub const fn driving(&self) -> bool {
        self.driving
    }

    /// Numbering epoch of the last frame (`None` before the first).
    #[must_use]
    pub const fn epoch(&self) -> Option<u32> {
        self.epoch
    }

    /// Highest key `seq` the worker has applied.
    #[must_use]
    pub const fn input_ack(&self) -> u64 {
        self.input_ack
    }

    /// Frames applied so far.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// Frames dropped because they were older than one already applied.
    #[must_use]
    pub const fn superseded(&self) -> u64 {
        self.superseded
    }

    /// Lines scrolled up from the bottom (0 = following).
    #[must_use]
    pub const fn view_offset(&self) -> u64 {
        self.view_offset
    }

    /// Whether a full frame has been received since the last gap.
    #[must_use]
    pub const fn synced(&self) -> bool {
        !self.resync_pending && self.epoch.is_some()
    }

    /// Lines above the screen that can still be scrolled to.
    #[must_use]
    pub const fn history_len(&self) -> u64 {
        self.first_visible.0.saturating_sub(self.scrollback.oldest().0)
    }

    /// Apply one event.
    pub fn apply(&mut self, event: TermEvent) -> Vec<Effect> {
        match event {
            TermEvent::Frame(frame) => self.apply_frame(frame),
            TermEvent::Lines { start, lines } => {
                self.scrollback.insert_batch(start, lines);
                Vec::new()
            }
            TermEvent::Title(t) => {
                self.title = Some(t.clone());
                vec![Effect::Title(t)]
            }
            TermEvent::Cwd { path, repo } => {
                self.cwd = Some(path.clone());
                self.repo.clone_from(&repo);
                vec![Effect::Cwd { path, repo }]
            }
            TermEvent::Bell => vec![Effect::Bell],
            TermEvent::Notification { title, body } => vec![Effect::Notification { title, body }],
            TermEvent::Image { id, generation, width, height, rgba } => {
                let last = self.frames;
                let image = TermImage { generation, width, height, rgba: rgba.into(), last };
                self.keep_image(id, image);
                Vec::new()
            }
            TermEvent::ClipboardWrite { text } => vec![Effect::ClipboardWrite(text)],
            TermEvent::Colors(colors) => {
                self.colors = colors;
                Vec::new()
            }
            TermEvent::Exited { status } => {
                self.exited = Some(status);
                self.running = None;
                vec![Effect::Exited(status)]
            }
            TermEvent::Resized { cols, rows } => {
                self.size.cols = cols;
                self.size.rows = rows;
                self.screen.resize(cols, rows);
                Vec::new()
            }
            TermEvent::Driver { you } => {
                self.driving = you;
                Vec::new()
            }
            TermEvent::Error(e) => vec![Effect::Error(e)],
            TermEvent::Matches { needle, total, matches } => {
                vec![Effect::Matches { needle, total, matches }]
            }
            TermEvent::SearchInvalid { needle, message } => {
                vec![Effect::SearchInvalid { needle, message }]
            }
            TermEvent::Marker { id } => {
                vec![Effect::Request(TermRequest::Reached { marker: id })]
            }
        }
    }

    fn apply_frame(&mut self, frame: Frame) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.last_seq.is_some_and(|last| frame.seq < last || (frame.seq == last && !frame.full))
        {
            // The rest of a stream an attach replaced, arriving after the new one's frames:
            // what it carries is older than what is shown.
            self.superseded = self.superseded.saturating_add(1);
            return effects;
        }
        self.frames = self.frames.saturating_add(1);
        if self.epoch != Some(frame.epoch) {
            self.change_numbering(&frame);
        }
        let gap = match self.last_seq {
            Some(prev) => frame.seq != prev.wrapping_add(1),
            None => false,
        };
        if gap && !frame.full && !self.resync_pending {
            self.resync_pending = true;
            effects.push(Effect::Request(TermRequest::Attach { size: self.size }));
        }
        if frame.full {
            self.resync_pending = false;
        }
        self.last_seq = Some(frame.seq);
        if (frame.cols, frame.rows) != (self.screen.cols(), self.screen.rows()) {
            self.size.cols = frame.cols;
            self.size.rows = frame.rows;
            self.screen.resize(frame.cols, frame.rows);
        }
        let moved = self.first_visible != frame.first_visible_line;
        self.first_visible = frame.first_visible_line;
        self.scrollback.set_extent(frame.oldest_line, frame.total_lines);
        if moved && !frame.full {
            self.readopt();
        }
        for update in frame.updates {
            let index = self.first_visible.offset(u64::from(update.row));
            // One allocation held twice: the screen row and its scrollback entry are the same
            // line, so a row that scrolls into history is never copied.
            let line = Arc::new(update.line);
            self.scrollback.insert_shared(index, Arc::clone(&line));
            if let Err(e) = self.screen.apply_shared(update.row, line) {
                tracing::debug!(error = %e, "row update rejected");
            }
        }
        *self.screen.cursor_mut() = frame.cursor;
        self.screen.set_modes(frame.modes);
        for placement in &frame.images {
            if let Some(image) = self.images.get_mut(&placement.image) {
                image.last = self.frames;
            }
        }
        self.placements = frame.images;
        self.input_ack = frame.input_ack;
        self.track_command(&mut effects);
        // Keep the viewport anchored on content while scrolled (offset counts from the bottom, so
        // nothing to do); clamp if history shrank.
        self.view_offset = self.view_offset.min(self.history_len());
        effects
    }

    /// A frame in another numbering: the lines held are put aside, kept for the primary
    /// screen while the alternate one is up, and taken back when the worker returns to the
    /// numbering they were held under.
    fn change_numbering(&mut self, frame: &Frame) {
        let held = std::mem::replace(&mut self.scrollback, Scrollback::new(CACHE_LINES));
        let parked = self.parked.take();
        let to_alt = frame.modes.contains(TermModes::ALT_SCREEN)
            && !self.screen.modes().contains(TermModes::ALT_SCREEN);
        let latest_prompt = self.latest_prompt.take();
        if to_alt {
            self.parked = self.epoch.map(|epoch| Parked { epoch, scrollback: held, latest_prompt });
        } else if let Some(back) = parked.filter(|p| p.epoch == frame.epoch) {
            self.scrollback = back.scrollback;
            self.latest_prompt = back.latest_prompt;
        }
        self.epoch = Some(frame.epoch);
        self.view_offset = 0;
    }

    /// The screen moved down the numbering without being sent again (output scrolled it):
    /// every row shows the line held at its index, and the frame then replaces what changed.
    fn readopt(&mut self) {
        let cols = self.screen.cols();
        for row in 0..self.screen.rows() {
            let index = self.first_visible.offset(u64::from(row));
            let line = self
                .scrollback
                .shared(index)
                .filter(|l| l.cols() == cols)
                .unwrap_or_else(|| Arc::new(Line::blank(cols)));
            if let Err(e) = self.screen.apply_shared(row, line) {
                tracing::debug!(error = %e, "row not re-adopted");
            }
        }
    }

    /// Follow the shell's command blocks from the marks: a command is running once the cursor
    /// has left the rows it was typed on, and finished when a newer prompt starts (whose `exit`
    /// is its status). Reads the prompt's rows only, so it runs on every frame.
    fn track_command(&mut self, effects: &mut Vec<Effect>) {
        let Some(prompt) = self.newest_prompt() else { return };
        match self.latest_prompt {
            // The first prompt of an epoch (a reflow, a reset, the alt screen coming or going)
            // says nothing about what ran before it — unless a command was running: its
            // block is either still the newest (running on, under new numbers) or not (it
            // finished, and the newest prompt carries its status).
            None => {
                self.latest_prompt = Some(prompt);
                if let Some((_, command)) = &self.running {
                    let head = self.block_head(prompt);
                    if head.as_ref().and_then(|h| h.command.as_ref()) == Some(command) {
                        self.running = Some((prompt, command.clone()));
                    } else if let Some((_, command)) = self.running.take() {
                        let typed_at = self.prompt_before(prompt).filter(|&p| {
                            self.block_head(p).and_then(|h| h.command) == Some(command.clone())
                        });
                        let exit = head.and_then(|h| h.exit);
                        effects.push(Effect::CommandFinished { prompt: typed_at, command, exit });
                    }
                }
            }
            // Newer, or the screen was erased in place (⌃L at a prompt keeps the numbering
            // and redraws the prompt higher up): either way a prompt the shell just drew.
            Some(seen) if prompt != seen => {
                self.latest_prompt = Some(prompt);
                if let Some((typed_at, command)) = self.running.take() {
                    let exit = self.line(prompt).and_then(|l| l.mark.exit());
                    effects.push(Effect::CommandFinished { prompt: Some(typed_at), command, exit });
                }
            }
            Some(_) => {}
        }
        if self.running.is_some() {
            return;
        }
        // Idle at a prompt, the cursor is still on its rows: settled from the marks alone,
        // without reading the command's text.
        let cursor = self.first_visible.offset(u64::from(self.screen.cursor().row));
        if cursor.0 >= self.block_body(prompt).0
            && let Some(head) = self.block_head(prompt)
            && let Some(command) = head.command
        {
            self.running = Some((prompt, command.clone()));
            effects.push(Effect::CommandStarted(command));
        }
    }

    /// The newest prompt start on the screen.
    fn newest_prompt(&self) -> Option<LineIndex> {
        (0..self.screen.rows())
            .rev()
            .map(|r| self.first_visible.offset(u64::from(r)))
            .find(|&index| self.line(index).is_some_and(|l| l.mark.starts_prompt()))
    }

    /// Scroll the viewport by `delta` lines (positive = up into history). Returns fetch requests
    /// for lines not cached.
    pub fn scroll(&mut self, delta: i64) -> Vec<Effect> {
        let target = if delta >= 0 {
            self.view_offset.saturating_add(delta.unsigned_abs())
        } else {
            self.view_offset.saturating_sub(delta.unsigned_abs())
        };
        self.scroll_to(target)
    }

    /// Scroll to an absolute offset from the bottom.
    pub fn scroll_to(&mut self, offset: u64) -> Vec<Effect> {
        self.view_offset = offset.min(self.history_len());
        self.fetch_missing()
    }

    /// Jump back to following output.
    pub const fn scroll_to_bottom(&mut self) {
        self.view_offset = 0;
    }

    /// Fetch requests for the currently visible range.
    fn fetch_missing(&self) -> Vec<Effect> {
        if self.view_offset == 0 {
            return Vec::new();
        }
        let start = LineIndex(self.first_visible.0.saturating_sub(self.view_offset));
        self.scrollback
            .missing(start, u64::from(self.screen.rows()))
            .into_iter()
            .map(|(start, count)| {
                let count = u32::try_from(count).unwrap_or(u32::MAX);
                Effect::Request(TermRequest::FetchLines { start, count })
            })
            .collect()
    }

    /// Whether a command is running now: it left its prompt and no newer prompt has started
    /// (shell integration marks decide; without marks nothing is ever running).
    #[must_use]
    pub const fn command_running(&self) -> bool {
        self.running.is_some()
    }

    /// The command running now, as typed (rows joined with newlines); `None` when none is.
    #[must_use]
    pub fn running_command(&self) -> Option<&str> {
        self.running.as_ref().map(|(_, command)| command.as_str())
    }

    /// The last finished command: what was typed at the block before the newest prompt
    /// (shell integration marks it); `None` when no command has run.
    #[must_use]
    pub fn last_command(&self) -> Option<String> {
        let newest_prompt = self.prompt_before(LineIndex(self.newest().0.saturating_add(1)))?;
        let previous = self.prompt_before(newest_prompt)?;
        self.block_head(previous)?.command
    }

    /// Newest line (the bottom of the screen).
    fn newest(&self) -> LineIndex {
        LineIndex(
            self.first_visible.0.saturating_add(u64::from(self.screen.rows())).saturating_sub(1),
        )
    }

    /// The last finished command's block (the one before the newest prompt): what was typed
    /// and what it printed; `None` when no command has run.
    #[must_use]
    pub fn last_block(&self) -> Option<CommandBlock> {
        let newest_prompt = self.prompt_before(LineIndex(self.newest().0.saturating_add(1)))?;
        let previous = self.prompt_before(newest_prompt)?;
        self.command_block(previous)
    }

    /// The last `limit` distinct commands typed at this shell's prompts, newest first, among
    /// the lines held here (shell integration marks them; a repeat keeps its newest place).
    #[must_use]
    pub fn recent_commands(&self, limit: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut at = LineIndex(self.newest().0.saturating_add(1));
        while out.len() < limit {
            let Some(prompt) = self.prompt_before(at) else { break };
            if let Some(command) = self.block_head(prompt).and_then(|head| head.command)
                && !out.contains(&command)
            {
                out.push(command);
            }
            at = prompt;
        }
        out
    }

    /// The start of the nearest prompt strictly above `index`, among the lines held here
    /// (uncached history is not searched).
    #[must_use]
    pub fn prompt_before(&self, index: LineIndex) -> Option<LineIndex> {
        // The cache indexes its prompts: a walk here cost 20 000 lookups a frame in a
        // flooding shell whose prompt had been evicted (MEASUREMENTS 2026-09-13).
        self.scrollback.prompt_before(index)
    }

    /// The start of the nearest prompt strictly below `index`, among the lines held here.
    #[must_use]
    pub fn prompt_after(&self, index: LineIndex) -> Option<LineIndex> {
        self.scrollback.prompt_after(index).filter(|&p| p <= self.newest())
    }

    /// The head of the command block a line belongs to: its prompt row and the typed command
    /// (from the prompt row's input column through any further `Input` rows), read from the
    /// prompt's rows alone. `None` off a block (before the first prompt, or no shell
    /// integration).
    #[must_use]
    pub fn block_head(&self, index: LineIndex) -> Option<BlockHead> {
        let prompt = if self.line(index).is_some_and(|l| l.mark.starts_prompt()) {
            index
        } else {
            self.prompt_before(index)?
        };
        let exit = self.line(prompt)?.mark.exit();
        let body = self.block_body(prompt);
        // From each prompt row its input column on; an `Input` row whole.
        let command: Vec<String> = (prompt.0..body.0)
            .filter_map(|i| {
                let line = self.line(LineIndex(i))?;
                let typed = match line.mark.input_col() {
                    Some(col) => line.text_from(col),
                    None if line.mark == SemanticMark::Input => line.text(),
                    None => return None,
                };
                Some(typed.trim_end().to_owned())
            })
            .collect();
        let command = command.join("\n");
        Some(BlockHead {
            prompt,
            exit,
            command: (!command.trim().is_empty()).then_some(command),
            body,
        })
    }

    /// The first line after the rows a command was typed on: the prompt's rows (a start, then
    /// continuations) and any `Input` rows continuing a multi-line command. Reads the marks
    /// only, so it costs nothing to ask on every frame.
    fn block_body(&self, prompt: LineIndex) -> LineIndex {
        let newest = self.newest().0;
        let mut i = prompt.0;
        while i <= newest {
            let typed_on = self.line(LineIndex(i)).is_some_and(|line| {
                (line.mark.is_prompt() && (i == prompt.0 || !line.mark.starts_prompt()))
                    || line.mark == SemanticMark::Input
            });
            if !typed_on {
                break;
            }
            i = i.saturating_add(1);
        }
        LineIndex(i)
    }

    /// The arrow keys that take the shell's cursor to a click at (`index`, `col`): rows
    /// (positive = down) then cells (positive = right), as ghostty's `cursor-click-to-move`.
    ///
    /// `None` when the click would not land in the shell's line editor: the alternate
    /// screen, a program reading the mouse, a hidden cursor, or a row between the cursor's
    /// and the clicked one that is not part of the typed input (the prompt's own text, a
    /// command's output). The click is held to the input: not before its row's input column,
    /// not past its text. Soft-wrapped rows are one line to the shell, so a click on another
    /// wrapped row of the cursor's line is a cell count; a hard row boundary (a command
    /// continued on the next line) is a row step, with the cell count taken from each line's
    /// start, as the shell's ↑ / ↓ keep the column.
    #[must_use]
    pub fn cursor_path_to(&self, index: LineIndex, col: u16) -> Option<(i32, i32)> {
        let modes = self.modes();
        if modes.intersects(
            TermModes::ALT_SCREEN | TermModes::MOUSE_TRACKING | TermModes::CURSOR_HIDDEN,
        ) {
            return None;
        }
        let cursor = self.cursor();
        let from = self.index_at_row(cursor.row);
        let (lo, hi) = if from <= index { (from, index) } else { (index, from) };
        let mut i = lo.0;
        while i <= hi.0 {
            input_start(self.line(LineIndex(i))?)?;
            i = i.saturating_add(1);
        }
        let target = self.line(index)?;
        let first = input_start(target)?;
        let end = target.last_content_col().map_or(0, |c| c.saturating_add(1));
        // The spacer of a wide character is the character.
        let on_spacer =
            target.cells.get(usize::from(col)).is_some_and(|c| c.width == CellWidth::SpacerTail);
        let col = if on_spacer { col.saturating_sub(1) } else { col };
        let col = col.clamp(first, end.max(first));
        let rows = i32::try_from(self.logical_line_of(index))
            .ok()?
            .checked_sub(i32::try_from(self.logical_line_of(from)).ok()?)?;
        let cells = self
            .cells_into_line(index, col)?
            .checked_sub(self.cells_into_line(from, cursor.col)?)?;
        Some((rows, cells))
    }

    /// The row the logical line holding `index` starts on: back over soft-wrapped rows.
    fn logical_line_of(&self, index: LineIndex) -> u64 {
        let mut i = index.0;
        while i > 0 && self.line(LineIndex(i)).is_some_and(|l| l.flags.contains(LineFlags::WRAPPED))
        {
            i = i.saturating_sub(1);
        }
        i
    }

    /// Characters from the start of the typed input on the logical line holding `index` up
    /// to `col` on that row: wide characters once, their spacer cells not at all.
    fn cells_into_line(&self, index: LineIndex, col: u16) -> Option<i32> {
        let start = self.logical_line_of(index);
        let mut count = 0_i32;
        let mut i = start;
        while i <= index.0 {
            let line = self.line(LineIndex(i))?;
            let from = if i == start { input_start(line)? } else { 0 };
            let to = if i == index.0 { col } else { u16::try_from(line.cells.len()).ok()? };
            let drawn = line
                .cells
                .iter()
                .skip(usize::from(from))
                .take(usize::from(to.saturating_sub(from)))
                .filter(|c| c.width.draws_text())
                .count();
            count = count.checked_add(i32::try_from(drawn).ok()?)?;
            i = i.saturating_add(1);
        }
        Some(count)
    }

    /// The command block a line belongs to: its head ([`Self::block_head`]) and its output
    /// (the rows from the command's end to the next prompt, trailing blank rows trimmed),
    /// among the lines held here. Reads the whole block: not for every frame.
    #[must_use]
    pub fn command_block(&self, index: LineIndex) -> Option<CommandBlock> {
        let head = self.block_head(index)?;
        let end = self
            .prompt_after(head.prompt)
            .unwrap_or_else(|| LineIndex(self.newest().0.saturating_add(1)));
        let mut output: Vec<String> = Vec::new();
        let mut i = head.body.0;
        while i < end.0 {
            match self.line(LineIndex(i)) {
                Some(line) => output.push(line.text()),
                None => break,
            }
            i = i.saturating_add(1);
        }
        while output.last().is_some_and(String::is_empty) {
            output.pop();
        }
        Some(CommandBlock {
            prompt: head.prompt,
            end,
            exit: head.exit,
            command: head.command,
            output: output.join("\n"),
        })
    }

    /// Scroll so that `index` is the top row of the viewport.
    pub fn scroll_to_line(&mut self, index: LineIndex) -> Vec<Effect> {
        self.scroll_to(self.first_visible.0.saturating_sub(index.0))
    }

    /// The output of the last finished command: the output rows right above the newest prompt,
    /// trailing blank rows trimmed, joined with newlines. `None` without shell integration.
    #[must_use]
    pub fn last_command_output(&self) -> Option<String> {
        let prompt = self.prompt_before(LineIndex(self.newest().0.saturating_add(1)))?;
        let mut rows: Vec<String> = Vec::new();
        let mut i = prompt.0;
        while i > 0 {
            i = i.saturating_sub(1);
            match self.line(LineIndex(i)) {
                Some(line) if matches!(line.mark, SemanticMark::Output | SemanticMark::Unknown) => {
                    rows.push(line.text());
                }
                _ => break,
            }
        }
        rows.reverse();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        (!rows.is_empty()).then(|| rows.join("\n"))
    }

    /// The absolute index of view row `row` (0 = top of the viewport).
    #[must_use]
    pub const fn index_at_row(&self, row: u16) -> LineIndex {
        LineIndex(self.first_visible.0.saturating_sub(self.view_offset).saturating_add(row as u64))
    }

    /// The line at an absolute index: on screen, or in the scrollback cache.
    #[must_use]
    pub fn line(&self, index: LineIndex) -> Option<&Line> {
        if index >= self.first_visible {
            let row =
                usize::try_from(index.0.saturating_sub(self.first_visible.0)).unwrap_or(usize::MAX);
            self.screen.lines().get(row).map(AsRef::as_ref)
        } else {
            self.scrollback.get(index)
        }
    }

    /// The rows to draw, top to bottom.
    #[must_use]
    pub fn view(&self) -> Vec<ViewRow<'_>> {
        let rows = u64::from(self.screen.rows());
        if self.view_offset == 0 {
            return self
                .screen
                .lines()
                .iter()
                .enumerate()
                .map(|(i, line)| ViewRow {
                    index: self.first_visible.offset(u64::try_from(i).unwrap_or(u64::MAX)),
                    line: Some(line.as_ref()),
                })
                .collect();
        }
        let start = self.first_visible.0.saturating_sub(self.view_offset);
        (0..rows)
            .map(|i| {
                let index = LineIndex(start.saturating_add(i));
                let line = if index >= self.first_visible {
                    let row = usize::try_from(index.0.saturating_sub(self.first_visible.0))
                        .unwrap_or(usize::MAX);
                    self.screen.lines().get(row).map(AsRef::as_ref)
                } else {
                    self.scrollback.get(index)
                };
                ViewRow { index, line }
            })
            .collect()
    }

    /// The client's size changed: resize locally for immediate feedback and ask the worker.
    pub fn resize(&mut self, size: TermSize) -> Vec<Effect> {
        self.size = size;
        self.screen.resize(size.cols, size.rows);
        vec![Effect::Request(TermRequest::Resize(size))]
    }
}

/// The column the typed input starts at on a row of the shell's line editor: the prompt's
/// input column, 0 on a row the shell marked as input, `None` elsewhere.
const fn input_start(line: &Line) -> Option<u16> {
    match line.mark {
        SemanticMark::Input => Some(0),
        mark => mark.input_col(),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use slopty_grid::{Cell, RowUpdate, Style};

    use super::*;

    /// Applying a frame puts the row in the screen and in the scrollback as **one**
    /// allocation: a line that scrolls off is moved into history, never copied.
    #[test]
    fn an_applied_row_is_one_allocation_in_both_places() {
        let mut s = TermState::new(size());
        s.apply(TermEvent::Frame(frame(1, true, 0, 100, 103, &[(0, "one"), (1, "two")])));

        let on_screen = Arc::clone(s.screen().lines().first().expect("row 0"));
        let in_history = s.scrollback().shared(LineIndex(100)).expect("cached at its index");
        assert!(
            Arc::ptr_eq(&on_screen, &in_history),
            "the screen row and the history entry are the same line"
        );
        assert_eq!(on_screen.text(), "one");
        // Two owners inside the state, plus the two clones this test is holding.
        assert_eq!(Arc::strong_count(&on_screen), 4);
    }

    fn frame(
        seq: u64,
        full: bool,
        epoch: u32,
        first: u64,
        total: u64,
        rows: &[(u16, &str)],
    ) -> Frame {
        Frame {
            seq,
            full,
            epoch,
            cols: 10,
            rows: 3,
            cursor: Cursor::default(),
            modes: TermModes::empty(),
            oldest_line: LineIndex(0),
            first_visible_line: LineIndex(first),
            total_lines: total,
            input_ack: seq,
            images: Vec::new(),
            updates: rows
                .iter()
                .map(|(row, text)| RowUpdate {
                    row: *row,
                    line: Line::from_text(text, 10, Style::DEFAULT),
                })
                .collect(),
        }
    }

    fn size() -> TermSize {
        TermSize { cols: 10, rows: 3, ..TermSize::default() }
    }

    fn marked(text: &str, mark: SemanticMark) -> Line {
        let mut line = Line::from_text(text, 10, Style::DEFAULT);
        line.mark = mark;
        line
    }

    /// Every event that is not a frame is kept where the view reads it and re-emitted as its
    /// effect: title, cwd with its repository, the bell, a clipboard write, the exit (which
    /// also ends a running command), an error, matches and a refused pattern; a resize and
    /// the driver flag change the state and emit nothing.
    #[test]
    fn events_are_kept_and_re_emitted_as_effects() {
        let mut state = TermState::new(size());
        assert_eq!(state.apply(TermEvent::Title("vim".into())), vec![Effect::Title("vim".into())]);
        assert_eq!(state.title(), Some("vim"));
        let cwd = TermEvent::Cwd { path: "/w/app".into(), repo: Some("/w".into()) };
        assert_eq!(
            state.apply(cwd),
            vec![Effect::Cwd { path: "/w/app".into(), repo: Some("/w".into()) }]
        );
        assert_eq!((state.cwd(), state.repo()), (Some("/w/app"), Some("/w")));
        assert_eq!(state.apply(TermEvent::Bell), vec![Effect::Bell]);
        assert_eq!(
            state.apply(TermEvent::Notification { title: "T".to_owned(), body: "b".to_owned() }),
            vec![Effect::Notification { title: "T".to_owned(), body: "b".to_owned() }]
        );
        assert_eq!(
            state.apply(TermEvent::ClipboardWrite { text: "copied".into() }),
            vec![Effect::ClipboardWrite("copied".into())]
        );
        assert_eq!(
            state.apply(TermEvent::Error("lost".into())),
            vec![Effect::Error("lost".into())]
        );
        let colors = ColorOverrides { bg: Some([0x28, 0x2c, 0x34]), ..ColorOverrides::default() };
        assert_eq!(state.apply(TermEvent::Colors(colors.clone())), vec![]);
        assert_eq!(state.colors(), &colors, "the program's colours are kept for the painter");
        let hit = SearchMatch { line: LineIndex(4), col: 2, len: 3 };
        assert_eq!(
            state.apply(TermEvent::Matches { needle: "x".into(), total: 1, matches: vec![hit] }),
            vec![Effect::Matches { needle: "x".into(), total: 1, matches: vec![hit] }]
        );
        assert_eq!(
            state.apply(TermEvent::SearchInvalid { needle: "(".into(), message: "open".into() }),
            vec![Effect::SearchInvalid { needle: "(".into(), message: "open".into() }]
        );
        assert!(state.apply(TermEvent::Resized { cols: 20, rows: 5 }).is_empty());
        assert_eq!((state.size().cols, state.size().rows), (20, 5));
        assert_eq!((state.screen().cols(), state.screen().rows()), (20, 5));
        assert!(state.apply(TermEvent::Driver { you: true }).is_empty());
        assert!(state.driving());
        // A running command ends with the child.
        state.running = Some((LineIndex(0), "sleep 9".into()));
        assert_eq!(state.apply(TermEvent::Exited { status: 3 }), vec![Effect::Exited(3)]);
        assert_eq!(state.exited(), Some(3));
        assert!(state.running.is_none());
    }

    /// The input column is a cell: a wide glyph in the prompt (a starship's, a Powerline
    /// segment) takes two, so the command is read from its cell and not from a character
    /// count (which lost the command's first letter for every such prompt).
    #[test]
    fn a_prompt_with_a_wide_glyph_keeps_the_commands_first_letter() {
        let mut state = TermState::new(size());
        let mut f = frame(1, true, 0, 0, 3, &[(0, "  > ls")]);
        let line = &mut f.updates[0].line;
        line.cells[0] = Cell::wide("マ", Style::DEFAULT);
        line.cells[1] = Cell::spacer_tail(Style::DEFAULT);
        line.mark = SemanticMark::Prompt { exit: None, input: Some(4) };
        f.cursor.row = 1;
        assert_eq!(state.apply(TermEvent::Frame(f)), vec![Effect::CommandStarted("ls".to_owned())]);
        assert_eq!(state.block_head(LineIndex(0)).and_then(|h| h.command).as_deref(), Some("ls"));
    }

    #[test]
    fn a_command_is_reported_when_it_leaves_its_prompt_and_when_the_next_prompt_starts() {
        let prompt = |exit| SemanticMark::Prompt { exit, input: Some(2) };
        let mut state = TermState::new(size());
        let at = |mut f: Frame, row: u16| {
            f.cursor.row = row;
            f
        };
        let commands = |effects: Vec<Effect>| {
            effects
                .into_iter()
                .filter(|e| matches!(e, Effect::CommandStarted(_) | Effect::CommandFinished { .. }))
                .collect::<Vec<_>>()
        };
        // An empty prompt: nothing runs.
        let mut f = frame(1, true, 0, 0, 3, &[(0, "$ ")]);
        f.updates[0].line.mark = prompt(None);
        assert!(commands(state.apply(TermEvent::Frame(at(f, 0)))).is_empty());
        assert!(!state.command_running());
        // Typed but not entered: the cursor is still on the prompt row.
        let mut f = frame(2, false, 0, 0, 3, &[(0, "$ sleep 9")]);
        f.updates[0].line.mark = prompt(None);
        assert!(commands(state.apply(TermEvent::Frame(at(f, 0)))).is_empty());
        // Enter: the cursor left the command's rows.
        let f = frame(3, false, 0, 0, 3, &[(1, "")]);
        assert_eq!(
            commands(state.apply(TermEvent::Frame(at(f, 1)))),
            vec![Effect::CommandStarted("sleep 9".to_owned())]
        );
        // Still running: nothing new.
        let f = frame(4, false, 0, 0, 3, &[(1, "")]);
        assert!(commands(state.apply(TermEvent::Frame(at(f, 1)))).is_empty());
        assert!(state.command_running());
        // The next prompt carries the status.
        let mut f = frame(5, false, 0, 0, 3, &[(2, "$ ")]);
        f.updates[0].line.mark = prompt(Some(1));
        assert_eq!(
            commands(state.apply(TermEvent::Frame(at(f, 2)))),
            vec![Effect::CommandFinished {
                prompt: Some(LineIndex(0)),
                command: "sleep 9".to_owned(),
                exit: Some(1)
            }]
        );
        assert!(!state.command_running());
        // ⌃L at the prompt: the shell erases the screen in place and redraws its prompt on the
        // first row, a lower index than the one it replaces. The command typed there is
        // finished by the prompt below it as usual.
        let mut f = frame(6, true, 0, 0, 3, &[(0, "$ "), (1, ""), (2, "")]);
        f.updates[0].line.mark = prompt(None);
        assert!(commands(state.apply(TermEvent::Frame(at(f, 0)))).is_empty());
        let mut f = frame(7, false, 0, 0, 3, &[(0, "$ sleep 2")]);
        f.updates[0].line.mark = prompt(None);
        assert!(commands(state.apply(TermEvent::Frame(at(f, 0)))).is_empty());
        let f = frame(8, false, 0, 0, 3, &[(1, "")]);
        assert_eq!(
            commands(state.apply(TermEvent::Frame(at(f, 1)))),
            vec![Effect::CommandStarted("sleep 2".to_owned())]
        );
        let mut f = frame(9, false, 0, 0, 3, &[(1, "$ ")]);
        f.updates[0].line.mark = prompt(Some(0));
        assert_eq!(
            commands(state.apply(TermEvent::Frame(at(f, 1)))),
            vec![Effect::CommandFinished {
                prompt: Some(LineIndex(0)),
                command: "sleep 2".to_owned(),
                exit: Some(0)
            }]
        );
        // A new epoch forgets which prompt was newest, so its first prompt ends nothing.
        let mut f = frame(10, true, 1, 0, 3, &[(0, "$ vim"), (1, "$ ")]);
        f.updates[0].line.mark = prompt(None);
        f.updates[1].line.mark = prompt(Some(0));
        assert!(commands(state.apply(TermEvent::Frame(at(f, 1)))).is_empty());
    }

    /// The numbering changes under a running command (the window was resized, so the worker
    /// reflowed): while its block is still the newest it runs on under the new numbers, and
    /// once a newer prompt exists it finished, with that prompt's status and its own new row
    /// when it is held here.
    #[test]
    fn a_running_command_survives_a_reflow_and_finishes_after_one() {
        let prompt = |exit| SemanticMark::Prompt { exit, input: Some(2) };
        let mut state = TermState::new(size());
        let at = |mut f: Frame, row: u16| {
            f.cursor.row = row;
            f
        };
        let commands = |effects: Vec<Effect>| {
            effects
                .into_iter()
                .filter(|e| matches!(e, Effect::CommandStarted(_) | Effect::CommandFinished { .. }))
                .collect::<Vec<_>>()
        };
        let mut f = frame(1, true, 0, 0, 3, &[(0, "$ sleep 9"), (1, ""), (2, "")]);
        f.updates[0].line.mark = prompt(None);
        assert_eq!(
            commands(state.apply(TermEvent::Frame(at(f, 1)))),
            vec![Effect::CommandStarted("sleep 9".to_owned())]
        );
        // Reflowed: the same block, one row further down, still the newest.
        let mut f = frame(2, true, 1, 0, 3, &[(0, ""), (1, "$ sleep 9"), (2, "")]);
        f.updates[1].line.mark = prompt(None);
        assert!(commands(state.apply(TermEvent::Frame(at(f, 2)))).is_empty());
        assert!(state.command_running(), "runs on under the new numbers");
        assert_eq!(state.running.as_ref().map(|(p, _)| *p), Some(LineIndex(1)));
        // Reflowed again, and this time the shell has printed the next prompt.
        let mut f = frame(3, true, 2, 0, 3, &[(0, "$ sleep 9"), (1, "$ "), (2, "")]);
        f.updates[0].line.mark = prompt(None);
        f.updates[1].line.mark = prompt(Some(3));
        assert_eq!(
            commands(state.apply(TermEvent::Frame(at(f, 1)))),
            vec![Effect::CommandFinished {
                prompt: Some(LineIndex(0)),
                command: "sleep 9".to_owned(),
                exit: Some(3)
            }]
        );
        assert!(!state.command_running());
        // A command whose prompt scrolled out of the rows held here still finishes; the
        // caption has no row to land on.
        let mut f = frame(4, true, 3, 0, 3, &[(0, "$ make"), (1, ""), (2, "")]);
        f.updates[0].line.mark = prompt(None);
        assert_eq!(
            commands(state.apply(TermEvent::Frame(at(f, 1)))),
            vec![Effect::CommandStarted("make".to_owned())]
        );
        let mut f = frame(5, true, 4, 10, 13, &[(0, "out"), (1, "$ "), (2, "")]);
        f.updates[0].line.mark = SemanticMark::Output;
        f.updates[1].line.mark = prompt(Some(0));
        assert_eq!(
            commands(state.apply(TermEvent::Frame(at(f, 1)))),
            vec![Effect::CommandFinished {
                prompt: None,
                command: "make".to_owned(),
                exit: Some(0)
            }]
        );
    }

    #[test]
    fn prompt_navigation_and_last_output_follow_the_marks() {
        let prompt = |exit| SemanticMark::Prompt { exit, input: Some(2) };
        let mut state = TermState::new(size());
        // Screen 6..=8, then history 0..=5 into the cache.
        let mut f = frame(1, true, 0, 6, 9, &[(0, "2"), (1, ""), (2, "$ ")]);
        f.updates[0].line.mark = SemanticMark::Output;
        f.updates[1].line.mark = SemanticMark::Output;
        f.updates[2].line.mark = prompt(Some(0));
        state.apply(TermEvent::Frame(f));
        state.apply(TermEvent::Lines {
            start: LineIndex(0),
            lines: vec![
                marked("$ ls", prompt(None)),
                marked("a", SemanticMark::Output),
                marked("b", SemanticMark::Output),
                marked("$ false", prompt(Some(0))),
                marked("$ seq 2", prompt(Some(1))),
                marked("1", SemanticMark::Output),
            ],
        });

        assert_eq!(state.prompt_before(LineIndex(8)), Some(LineIndex(4)));
        assert_eq!(state.prompt_before(LineIndex(4)), Some(LineIndex(3)));
        assert_eq!(state.prompt_before(LineIndex(0)), None);
        assert_eq!(state.prompt_after(LineIndex(0)), Some(LineIndex(3)));
        assert_eq!(state.prompt_after(LineIndex(4)), Some(LineIndex(8)));
        assert_eq!(state.prompt_after(LineIndex(8)), None);
        assert_eq!(state.last_command_output(), Some("1\n2".to_owned()), "blank tail trimmed");
        assert_eq!(state.last_command().as_deref(), Some("seq 2"));
        assert_eq!(state.recent_commands(8), ["seq 2", "false", "ls"], "newest first");
        let last = state.last_block().expect("the seq block");
        assert_eq!((last.command.as_deref(), last.output.as_str()), (Some("seq 2"), "1\n2"));
        assert_eq!(state.recent_commands(2), ["seq 2", "false"], "the limit cuts the oldest");
        assert!(state.recent_commands(0).is_empty());
        // A block from any of its rows: the prompt, the typed command, the trimmed output.
        let block = state.command_block(LineIndex(6)).expect("the seq block");
        assert_eq!((block.prompt, block.end, block.exit), (LineIndex(4), LineIndex(8), Some(1)));
        assert_eq!(block.command.as_deref(), Some("seq 2"));
        assert_eq!(block.output, "1\n2");
        let block = state.command_block(LineIndex(3)).expect("the false block");
        assert_eq!((block.prompt, block.end), (LineIndex(3), LineIndex(4)));
        assert_eq!((block.command.as_deref(), block.output.as_str()), (Some("false"), ""));
        assert_eq!(state.command_block(LineIndex(0)).map(|b| b.output), Some("a\nb".to_owned()));
        let newest = state.command_block(LineIndex(8)).expect("the open prompt");
        assert_eq!((newest.prompt, newest.end), (LineIndex(8), LineIndex(9)));
        assert_eq!(newest.command, None, "nothing typed after the prompt");
        // A command typed twice is listed once, at its newest place.
        state.apply(TermEvent::Lines {
            start: LineIndex(3),
            lines: vec![marked("$ ls", prompt(Some(0)))],
        });
        assert_eq!(state.recent_commands(8), ["seq 2", "ls"], "the repeat keeps its newest place");
        // The head alone, read every frame by the sticky header: no output is gathered.
        let head = state.block_head(LineIndex(6)).expect("the seq head");
        assert_eq!((head.prompt, head.body, head.exit), (LineIndex(4), LineIndex(5), Some(1)));
        assert_eq!(head.command.as_deref(), Some("seq 2"));
        assert_eq!(state.block_head(LineIndex(8)).map(|h| h.body), Some(LineIndex(9)));
        let _fetches: Vec<Effect> = state.scroll_to_line(LineIndex(3));
        assert_eq!(state.index_at_row(0), LineIndex(3));
        assert_eq!(state.view_offset(), 3);
    }

    /// A command continued on the next row (a `for` loop typed over two lines) is marked as
    /// input by the shell: the head joins it and its body starts below.
    #[test]
    fn a_continuation_row_joins_the_command_of_its_head() {
        let prompt = |exit| SemanticMark::Prompt { exit, input: Some(2) };
        let mut state = TermState::new(size());
        let mut f = frame(1, true, 0, 0, 3, &[(0, "$ for x"), (1, "do echo"), (2, "$ ")]);
        f.updates[0].line.mark = prompt(None);
        f.updates[1].line.mark = SemanticMark::Input;
        f.updates[2].line.mark = prompt(Some(0));
        state.apply(TermEvent::Frame(f));
        let head = state.block_head(LineIndex(0)).expect("the loop's head");
        assert_eq!(head.command.as_deref(), Some("for x\ndo echo"));
        assert_eq!(head.body, LineIndex(2));
    }

    /// A click on the input line is a path of arrow keys from the cursor; off the input
    /// (the prompt's text, output, another program's screen) it is nothing.
    #[test]
    fn a_click_on_the_input_is_a_path_for_the_cursor() {
        let prompt = |exit| SemanticMark::Prompt { exit, input: Some(2) };
        let mut state = TermState::new(size());
        let mut f = frame(1, true, 0, 0, 3, &[(0, "out"), (1, "$ ab\u{4f60}ccd"), (2, "efg")]);
        f.updates[1].line.mark = prompt(Some(0));
        // `from_text` knows no widths: the CJK character takes the two cells at 4 and 5.
        f.updates[1].line.cells[4] = Cell::wide("\u{4f60}", Style::DEFAULT);
        f.updates[1].line.cells[5] = Cell::spacer_tail(Style::DEFAULT);
        f.updates[2].line.mark = SemanticMark::Input;
        f.updates[2].line.flags |= LineFlags::WRAPPED;
        f.cursor = Cursor { row: 1, col: 4, visible: true, ..Cursor::default() };
        state.apply(TermEvent::Frame(f.clone()));
        let at = |state: &TermState, row, col| state.cursor_path_to(LineIndex(row), col);
        assert_eq!(at(&state, 1, 2), Some((0, -2)), "to the input's first cell");
        assert_eq!(at(&state, 1, 0), Some((0, -2)), "the prompt's text is not input: held to it");
        assert_eq!(at(&state, 1, 4), Some((0, 0)), "where the cursor is");
        assert_eq!(
            at(&state, 1, 5),
            Some((0, 0)),
            "the spacer of a wide character is the character"
        );
        assert_eq!(at(&state, 1, 6), Some((0, 1)), "past the wide character: one key, two cells");
        assert_eq!(at(&state, 1, 9), Some((0, 3)), "beyond the text: held to its end");
        assert_eq!(at(&state, 2, 1), Some((0, 6)), "a wrapped row is the same line to the shell");
        assert_eq!(at(&state, 2, 9), Some((0, 8)));
        assert_eq!(at(&state, 0, 1), None, "output is not input");

        // A hard continuation (a `for` typed over two rows) is a row step.
        let mut hard = f.clone();
        hard.updates[2].line.flags = LineFlags::empty();
        state.apply(TermEvent::Frame(hard));
        assert_eq!(
            at(&state, 2, 1),
            Some((1, -1)),
            "down a row, the column kept from each line's start"
        );
        assert_eq!(at(&state, 2, 3), Some((1, 1)));

        // The cursor on an output row (a program running) or the modes say no.
        let mut running = f.clone();
        running.cursor = Cursor { row: 0, col: 3, visible: true, ..Cursor::default() };
        state.apply(TermEvent::Frame(running));
        assert_eq!(at(&state, 1, 3), None, "the cursor is not in the line editor");
        for mode in [TermModes::ALT_SCREEN, TermModes::MOUSE_TRACKING, TermModes::CURSOR_HIDDEN] {
            let mut moded = f.clone();
            moded.modes = mode;
            state.apply(TermEvent::Frame(moded));
            assert_eq!(at(&state, 1, 2), None, "{mode:?}");
        }
    }

    /// Output that begins on the very first line (a shell attached mid-command) is all of the
    /// last command's output: the walk back stops at the top, not one short of it.
    #[test]
    fn output_from_the_first_line_is_the_last_commands_whole_output() {
        let mut state = TermState::new(size());
        let mut f = frame(1, true, 0, 0, 3, &[(0, "a"), (1, "b"), (2, "$ ")]);
        f.updates[2].line.mark = SemanticMark::Prompt { exit: Some(0), input: Some(2) };
        state.apply(TermEvent::Frame(f));
        assert_eq!(state.last_command_output(), Some("a\nb".to_owned()));
    }

    fn texts(state: &TermState) -> Vec<Option<String>> {
        state.view().into_iter().map(|r| r.line.map(Line::text)).collect()
    }

    #[test]
    fn the_state_counts_its_frames_and_reads_back_the_epoch_and_the_ack() {
        let mut s = TermState::new(size());
        assert!(!s.driving(), "a client drives only when the worker says so");
        assert_eq!((s.epoch(), s.input_ack(), s.frames()), (None, 0, 0));
        s.apply(TermEvent::Frame(frame(7, true, 3, 0, 1, &[(0, "a")])));
        assert_eq!((s.epoch(), s.input_ack(), s.frames()), (Some(3), 7, 1));
        s.apply(TermEvent::Frame(frame(9, false, 3, 0, 1, &[(0, "b")])));
        assert_eq!((s.epoch(), s.input_ack(), s.frames()), (Some(3), 9, 2));
    }

    #[test]
    fn frames_fill_screen_and_cache_history() {
        let mut s = TermState::new(size());
        assert!(
            s.apply(TermEvent::Frame(frame(1, true, 0, 0, 3, &[(0, "a"), (1, "b"), (2, "c")])))
                .is_empty()
        );
        // Output scrolled by two lines: rows now hold c, d, e at absolute 2..5.
        s.apply(TermEvent::Frame(frame(2, false, 0, 2, 5, &[(0, "c"), (1, "d"), (2, "e")])));
        assert_eq!(texts(&s), vec![Some("c".into()), Some("d".into()), Some("e".into())]);
        assert_eq!(s.history_len(), 2);
        // Scroll up two lines: a, b come from the cache, c from the screen; nothing to fetch.
        assert!(s.scroll(2).is_empty());
        assert_eq!(texts(&s), vec![Some("a".into()), Some("b".into()), Some("c".into())]);
        assert_eq!(s.view_offset(), 2);
        assert!(s.scroll(5).is_empty(), "clamped to history");
        assert_eq!(s.view_offset(), 2);
        s.scroll_to_bottom();
        assert_eq!(s.view_offset(), 0, "following output again");
        assert_eq!(texts(&s), vec![Some("c".into()), Some("d".into()), Some("e".into())]);
    }

    #[test]
    fn missing_history_is_fetched_and_filled() {
        let mut s = TermState::new(size());
        // Attach mid-way: first visible is 100, history 0..100 exists on the worker.
        s.apply(TermEvent::Frame(frame(1, true, 0, 100, 103, &[(0, "x"), (1, "y"), (2, "z")])));
        let effects = s.scroll(3);
        assert_eq!(
            effects,
            vec![Effect::Request(TermRequest::FetchLines { start: LineIndex(97), count: 3 })]
        );
        assert_eq!(texts(&s), vec![None, None, None]);
        s.apply(TermEvent::Lines {
            start: LineIndex(97),
            lines: ["p", "q", "r"].iter().map(|t| Line::from_text(t, 10, Style::DEFAULT)).collect(),
        });
        assert_eq!(texts(&s), vec![Some("p".into()), Some("q".into()), Some("r".into())]);
    }

    #[test]
    fn seq_gap_requests_resync_once_and_full_frame_clears_it() {
        let mut s = TermState::new(size());
        s.apply(TermEvent::Frame(frame(1, true, 0, 0, 3, &[(0, "a")])));
        let effects = s.apply(TermEvent::Frame(frame(5, false, 0, 0, 3, &[(1, "b")])));
        assert_eq!(effects, vec![Effect::Request(TermRequest::Attach { size: size() })]);
        assert!(!s.synced());
        assert!(
            s.apply(TermEvent::Frame(frame(6, false, 0, 0, 3, &[(1, "b")]))).is_empty(),
            "no second request"
        );
        s.apply(TermEvent::Frame(frame(7, true, 0, 0, 3, &[(0, "a"), (1, "b"), (2, "")])));
        assert!(s.synced());
    }

    #[test]
    fn images_are_kept_for_their_placements_and_the_oldest_placed_go_first() {
        use slopty_proto::terminal::PixelRect;
        let mut t = TermState::new(TermSize::default());
        let image = |id: u32, generation: u64, bytes: usize| TermEvent::Image {
            id,
            generation,
            width: 1,
            height: 1,
            rgba: vec![7; bytes],
        };
        let placement = |image: u32, generation: u64| Placement {
            image,
            generation,
            col: 0,
            row: 0,
            cols: 1,
            rows: 1,
            x_offset: 0,
            y_offset: 0,
            width: 1,
            height: 1,
            source: PixelRect { x: 0, y: 0, width: 1, height: 1 },
            z: 0,
        };
        assert!(t.apply(image(1, 1, 4)).is_empty());
        let mut first = frame(1, true, 0, 0, 3, &[]);
        first.images = vec![placement(1, 1)];
        let _effects = t.apply(TermEvent::Frame(first));
        assert_eq!(t.placements().len(), 1);
        assert_eq!(t.image(&placement(1, 1)).map(|i| i.rgba.to_vec()), Some(vec![7; 4]));
        assert_eq!(t.image(&placement(1, 2)), None, "a newer generation is not held");
        assert!(t.holds(1, 1) && !t.holds(1, 2) && !t.holds(2, 1), "what a refresh must ask for");
        // Image 2 arrives, then frame 2 places image 1 again: image 2 is now the least
        // recently placed, and the first to go when image 3 pushes the cache over budget.
        let half = IMAGE_CACHE_BYTES / 2;
        let _effects = t.apply(image(2, 1, half));
        let mut second = frame(2, false, 0, 0, 3, &[]);
        second.images = vec![placement(1, 1)];
        let _effects = t.apply(TermEvent::Frame(second));
        let _effects = t.apply(image(3, 1, half - 4));
        assert!(t.holds(2, 1), "exactly the budget is within it");
        let _effects = t.apply(image(3, 1, half + 1));
        assert_eq!(t.image(&placement(2, 1)), None);
        assert!(t.image(&placement(1, 1)).is_some(), "placed by the latest frame");
        assert!(t.image(&placement(3, 1)).is_some(), "just arrived");
    }

    #[test]
    fn epoch_change_drops_cache_and_scroll() {
        let mut s = TermState::new(size());
        s.apply(TermEvent::Frame(frame(1, true, 0, 0, 3, &[(0, "a"), (1, "b"), (2, "c")])));
        s.apply(TermEvent::Frame(frame(2, false, 0, 2, 5, &[(0, "c"), (1, "d"), (2, "e")])));
        s.scroll(2);
        s.apply(TermEvent::Frame(frame(3, true, 1, 2, 5, &[(0, "C"), (1, "D"), (2, "E")])));
        assert_eq!(s.view_offset(), 0);
        let fetch = s.scroll(2);
        assert_eq!(fetch.len(), 1, "old cache gone: {fetch:?}");
    }

    /// Output scrolled and the frame carries only the line that came in: the rows the
    /// client holds move up, taken from the lines kept by index.
    #[test]
    fn a_scroll_moves_the_held_rows_up_and_takes_only_the_new_one() {
        let mut s = TermState::new(size());
        s.apply(TermEvent::Frame(frame(1, true, 0, 0, 3, &[(0, "a"), (1, "b"), (2, "c")])));
        assert!(s.apply(TermEvent::Frame(frame(2, false, 0, 1, 4, &[(2, "d")]))).is_empty());
        assert_eq!(texts(&s), vec![Some("b".into()), Some("c".into()), Some("d".into())]);
        let shown: Vec<String> = s.screen().lines().iter().map(|l| l.text()).collect();
        assert_eq!(shown, ["b", "c", "d"]);
        // A line the client never held comes up blank rather than as another row's.
        s.apply(TermEvent::Lines { start: LineIndex(9), lines: Vec::new() });
        s.apply(TermEvent::Frame(frame(3, false, 0, 3, 6, &[(2, "f")])));
        let shown: Vec<String> = s.screen().lines().iter().map(|l| l.text()).collect();
        assert_eq!(shown, ["d", "", "f"]);
    }

    /// Frames from a stream a re-attach replaced can arrive after the new stream's: one older
    /// than the last applied is dropped, and so is a diff at its number; a joiner's full
    /// frame at the others' number is taken.
    #[test]
    fn a_frame_older_than_the_last_applied_is_dropped() {
        let mut s = TermState::new(size());
        s.apply(TermEvent::Frame(frame(4, false, 0, 0, 3, &[(0, "old")])));
        assert!(s.apply(TermEvent::Frame(frame(5, true, 0, 0, 3, &[(0, "new")]))).is_empty());
        for stale in [frame(5, false, 0, 0, 3, &[(0, "late")]), frame(3, true, 0, 0, 3, &[])] {
            assert!(s.apply(TermEvent::Frame(stale)).is_empty(), "no resync asked");
        }
        assert_eq!(s.superseded(), 2);
        assert_eq!(s.screen().line(0).map(Line::text).as_deref(), Some("new"));
        s.apply(TermEvent::Frame(frame(5, true, 0, 0, 3, &[(0, "joined")])));
        assert_eq!(s.screen().line(0).map(Line::text).as_deref(), Some("joined"));
        assert!(s.apply(TermEvent::Frame(frame(6, false, 0, 0, 3, &[(1, "x")]))).is_empty());
        assert_eq!((s.frames(), s.superseded()), (4, 2));
    }

    /// A program on the alternate screen and back: the primary's lines are put aside and
    /// taken back when the worker returns to their numbering, so scrolling up after `vim`
    /// asks for nothing; a numbering the worker did not return to drops them.
    #[test]
    fn the_primary_lines_come_back_after_the_alternate_screen() {
        let alt = |f: Frame| Frame { modes: TermModes::ALT_SCREEN, ..f };
        let mut s = TermState::new(size());
        s.apply(TermEvent::Frame(frame(1, true, 4, 0, 3, &[(0, "a"), (1, "b"), (2, "c")])));
        s.apply(TermEvent::Frame(frame(2, false, 4, 2, 5, &[(1, "d"), (2, "$ vim")])));
        s.apply(TermEvent::Frame(alt(frame(3, true, 5, 0, 3, &[(0, "~"), (1, "~"), (2, "")]))));
        assert_eq!(
            s.scrollback().get(LineIndex(0)).map(Line::text).as_deref(),
            Some("~"),
            "the alternate screen's own lines"
        );
        s.apply(TermEvent::Frame(frame(4, true, 4, 2, 5, &[(0, "c"), (1, "d"), (2, "$ vim")])));
        assert!(s.scroll(2).is_empty(), "the history is held again");
        assert_eq!(texts(&s), vec![Some("a".into()), Some("b".into()), Some("c".into())]);
        s.apply(TermEvent::Frame(alt(frame(5, true, 6, 0, 3, &[(0, "~")]))));
        s.apply(TermEvent::Frame(frame(6, true, 7, 2, 5, &[(0, "C"), (1, "D"), (2, "$ ")])));
        assert_eq!(s.scroll(2).len(), 1, "a new numbering: nothing taken back");
    }

    /// The worker asks, after enough frames, whether they arrived; the answer names the marker.
    #[test]
    fn a_marker_is_answered_once_the_events_before_it_are_applied() {
        let mut s = TermState::new(size());
        assert_eq!(
            s.apply(TermEvent::Marker { id: 41 }),
            vec![Effect::Request(TermRequest::Reached { marker: 41 })]
        );
    }

    #[test]
    fn a_gap_asks_for_a_resync_once_and_a_full_frame_never_does() {
        let mut s = TermState::new(size());
        assert!(s.apply(TermEvent::Frame(frame(1, true, 0, 0, 1, &[(0, "a")]))).is_empty());
        // A delta after a missed frame asks to attach again, at this size.
        let effects = s.apply(TermEvent::Frame(frame(3, false, 0, 0, 1, &[(0, "b")])));
        assert_eq!(effects, vec![Effect::Request(TermRequest::Attach { size: s.size() })]);
        // Another gap while that is pending asks nothing more.
        assert!(s.apply(TermEvent::Frame(frame(6, false, 0, 0, 1, &[(0, "c")]))).is_empty());
        // The full frame that answers it clears the flag, and a full frame after a gap never
        // asks: it is the resync.
        assert!(s.apply(TermEvent::Frame(frame(9, true, 0, 0, 1, &[(0, "d")]))).is_empty());
        assert!(s.apply(TermEvent::Frame(frame(20, true, 0, 0, 1, &[(0, "e")]))).is_empty());
        // A frame at another size resizes the screen; one at the same size leaves it alone.
        let wide = Frame { cols: 20, rows: 4, ..frame(21, false, 0, 0, 1, &[(0, "f")]) };
        s.apply(TermEvent::Frame(wide));
        assert_eq!((s.screen().cols(), s.screen().rows()), (20, 4));
        assert_eq!((s.size().cols, s.size().rows), (20, 4));
    }

    #[test]
    fn resize_event_and_request() {
        let mut s = TermState::new(size());
        let bigger = TermSize { cols: 20, rows: 5, ..TermSize::default() };
        assert_eq!(s.resize(bigger), vec![Effect::Request(TermRequest::Resize(bigger))]);
        assert_eq!((s.screen().cols(), s.screen().rows()), (20, 5));
        s.apply(TermEvent::Resized { cols: 8, rows: 2 });
        assert_eq!((s.screen().cols(), s.screen().rows()), (8, 2));
    }
}
