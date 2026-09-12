//! Terminal session state on the client.

use slopty_grid::{Cursor, Line, LineIndex, Screen, Scrollback, SemanticMark, TermModes};
use slopty_proto::terminal::{Frame, SearchMatch, TermEvent, TermRequest, TermSize};

/// Lines kept client-side. Newest-first eviction; the host retains 50k.
pub const CACHE_LINES: usize = 20_000;

/// What the UI or connection should do after an event was applied.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Effect {
    /// Send this to the host.
    Request(TermRequest),
    /// Ring the bell.
    Bell,
    /// The title changed.
    Title(String),
    /// The cwd changed, with the repository the host resolved it to.
    Cwd {
        /// The new directory.
        path: String,
        /// Its repository root, if it is in one.
        repo: Option<String>,
    },
    /// The program wrote to the clipboard.
    ClipboardWrite(String),
    /// The child exited.
    Exited(i32),
    /// The host reported an error for a request.
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
        }
    }

    /// Current size.
    #[must_use]
    pub const fn size(&self) -> TermSize {
        self.size
    }

    /// The scrollback cache: what is held, and how much of it the host still has.
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

    /// The repository the cwd is in, as the host resolved it.
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

    /// Highest key `seq` the host has applied.
    #[must_use]
    pub const fn input_ack(&self) -> u64 {
        self.input_ack
    }

    /// Frames applied so far.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
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
            TermEvent::ClipboardWrite { text } => vec![Effect::ClipboardWrite(text)],
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
        }
    }

    fn apply_frame(&mut self, frame: Frame) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.frames = self.frames.saturating_add(1);
        if self.epoch != Some(frame.epoch) {
            // Numbering changed (reflow, reset, alt screen): the cache is meaningless now.
            self.scrollback = Scrollback::new(CACHE_LINES);
            self.epoch = Some(frame.epoch);
            self.view_offset = 0;
            self.latest_prompt = None;
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
        self.first_visible = frame.first_visible_line;
        self.scrollback.set_extent(frame.oldest_line, frame.total_lines);
        for update in frame.updates {
            let index = self.first_visible.offset(u64::from(update.row));
            // One allocation held twice: the screen row and its scrollback entry are the same
            // line, so a row that scrolls into history is never copied.
            let line = std::sync::Arc::new(update.line);
            self.scrollback.insert_shared(index, std::sync::Arc::clone(&line));
            if let Err(e) = self.screen.apply_shared(update.row, line) {
                tracing::debug!(error = %e, "row update rejected");
            }
        }
        *self.screen.cursor_mut() = frame.cursor;
        self.screen.set_modes(frame.modes);
        self.input_ack = frame.input_ack;
        self.track_command(&mut effects);
        // Keep the viewport anchored on content while scrolled (offset counts from the bottom, so
        // nothing to do); clamp if history shrank.
        self.view_offset = self.view_offset.min(self.history_len());
        effects
    }

    /// Follow the shell's command blocks from the marks: a command is running once the cursor
    /// has left the rows it was typed on, and finished when a newer prompt starts (whose `exit`
    /// is its status). Reads the prompt's rows only, so it runs on every frame.
    fn track_command(&mut self, effects: &mut Vec<Effect>) {
        let Some(prompt) = self.newest_prompt() else { return };
        match self.latest_prompt {
            // The first prompt of an epoch (a reflow, a reset, the alt screen coming or going)
            // says nothing about what ran before it.
            None => self.latest_prompt = Some(prompt),
            Some(seen) if prompt.0 > seen.0 => {
                self.latest_prompt = Some(prompt);
                if let Some((_, command)) = self.running.take() {
                    let exit = self.line(prompt).and_then(|l| l.mark.exit());
                    effects.push(Effect::CommandFinished { command, exit });
                }
            }
            Some(_) => {}
        }
        if self.running.is_some() {
            return;
        }
        let cursor = self.first_visible.offset(u64::from(self.screen.cursor().row));
        if let Some(head) = self.block_head(prompt)
            && cursor.0 >= head.body.0
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

    /// The start of the nearest prompt strictly above `index`, among the lines held here
    /// (uncached history is not searched).
    #[must_use]
    pub fn prompt_before(&self, index: LineIndex) -> Option<LineIndex> {
        let oldest = self.scrollback.oldest().0.min(self.first_visible.0);
        let mut i = index.0;
        while i > oldest {
            i = i.saturating_sub(1);
            if self.line(LineIndex(i)).is_some_and(|l| l.mark.starts_prompt()) {
                return Some(LineIndex(i));
            }
        }
        None
    }

    /// The start of the nearest prompt strictly below `index`, among the lines held here.
    #[must_use]
    pub fn prompt_after(&self, index: LineIndex) -> Option<LineIndex> {
        let newest = self.newest().0;
        let mut i = index.0;
        while i < newest {
            i = i.saturating_add(1);
            if self.line(LineIndex(i)).is_some_and(|l| l.mark.starts_prompt()) {
                return Some(LineIndex(i));
            }
        }
        None
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
        let first = self.line(prompt)?;
        let exit = first.mark.exit();
        // The prompt's rows (a start, then continuations) up to the one the command was typed
        // on, from its input column; further `Input` rows continue a multi-line command.
        let mut command: Vec<String> = Vec::new();
        let mut i = prompt.0;
        let newest = self.newest().0;
        while i <= newest {
            match self.line(LineIndex(i)) {
                Some(line)
                    if line.mark.is_prompt() && (i == prompt.0 || !line.mark.starts_prompt()) =>
                {
                    if let Some(col) = line.mark.input_col() {
                        let typed: String = line.text().chars().skip(usize::from(col)).collect();
                        command.push(typed.trim_end().to_owned());
                    }
                }
                Some(line) if line.mark == SemanticMark::Input => {
                    command.push(line.text().trim_end().to_owned());
                }
                _ => break,
            }
            i = i.saturating_add(1);
        }
        let command = command.join("\n");
        Some(BlockHead {
            prompt,
            exit,
            command: (!command.trim().is_empty()).then_some(command),
            body: LineIndex(i),
        })
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

    /// The client's size changed: resize locally for immediate feedback and ask the host.
    pub fn resize(&mut self, size: TermSize) -> Vec<Effect> {
        self.size = size;
        self.screen.resize(size.cols, size.rows);
        vec![Effect::Request(TermRequest::Resize(size))]
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use slopty_grid::{RowUpdate, Style};

    use super::*;

    /// Applying a frame puts the row in the screen and in the scrollback as **one**
    /// allocation: a line that scrolls off is moved into history, never copied.
    #[test]
    fn an_applied_row_is_one_allocation_in_both_places() {
        let mut s = TermState::new(size());
        s.apply(TermEvent::Frame(frame(1, true, 0, 100, 103, &[(0, "one"), (1, "two")])));

        let on_screen = std::sync::Arc::clone(s.screen().lines().first().expect("row 0"));
        let in_history = s.scrollback().shared(LineIndex(100)).expect("cached at its index");
        assert!(
            std::sync::Arc::ptr_eq(&on_screen, &in_history),
            "the screen row and the history entry are the same line"
        );
        assert_eq!(on_screen.text(), "one");
        // Two owners inside the state, plus the two clones this test is holding.
        assert_eq!(std::sync::Arc::strong_count(&on_screen), 4);
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
        // The next prompt carries the status.
        let mut f = frame(5, false, 0, 0, 3, &[(2, "$ ")]);
        f.updates[0].line.mark = prompt(Some(1));
        assert_eq!(
            commands(state.apply(TermEvent::Frame(at(f, 2)))),
            vec![Effect::CommandFinished { command: "sleep 9".to_owned(), exit: Some(1) }]
        );
        // A new epoch forgets which prompt was newest, so its first prompt ends nothing.
        let mut f = frame(6, true, 1, 0, 3, &[(0, "$ vim"), (1, "$ ")]);
        f.updates[0].line.mark = prompt(None);
        f.updates[1].line.mark = prompt(Some(0));
        assert!(commands(state.apply(TermEvent::Frame(at(f, 1)))).is_empty());
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
        // The head alone, read every frame by the sticky header: no output is gathered.
        let head = state.block_head(LineIndex(6)).expect("the seq head");
        assert_eq!((head.prompt, head.body, head.exit), (LineIndex(4), LineIndex(5), Some(1)));
        assert_eq!(head.command.as_deref(), Some("seq 2"));
        assert_eq!(state.block_head(LineIndex(8)).map(|h| h.body), Some(LineIndex(9)));
        let _fetches: Vec<Effect> = state.scroll_to_line(LineIndex(3));
        assert_eq!(state.index_at_row(0), LineIndex(3));
        assert_eq!(state.view_offset(), 3);
    }

    fn texts(state: &TermState) -> Vec<Option<String>> {
        state.view().into_iter().map(|r| r.line.map(Line::text)).collect()
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
    }

    #[test]
    fn missing_history_is_fetched_and_filled() {
        let mut s = TermState::new(size());
        // Attach mid-way: first visible is 100, history 0..100 exists on the host.
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
