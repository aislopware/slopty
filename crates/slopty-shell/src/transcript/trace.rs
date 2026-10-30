//! Moving through the trace (`docs/decisions/inspector.md`, "One fold depth for the
//! whole trace", "Keys and find").
//!
//! - **One fold depth.** Prompts shows each turn's root row and its last reply; Steps, the default,
//!   shows groups folded and narration and subagent rows as they are; All opens every group. A
//!   depth sets what a fold is when the person never pressed it: the folds the person pressed keep
//!   their way at every depth, a depth change reorders nothing, and nothing opens or closes by
//!   itself.
//! - **Keys.** Where the trace holds the keyboard, ↑ and ↓ move a cursor over the steps shown (each
//!   row, and each step of an open group), scrolling only as far as it needs; ← folds what the
//!   cursor is on or goes up to its group, → opens it, Enter opens it or drills into a subagent,
//!   and E goes to the next failure (⇧E the one before) in the whole trace, opening what hides it
//!   (the depth, its group) to land on it. Esc is the inspector's: it goes back.

use gpui::{Context, KeyDownEvent, SharedString, Window};

use super::{Row, RowKind, ToolItem, Transcript};

/// How much of the trace shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Depth {
    /// Each turn's root row and its last reply.
    Prompts,
    /// Groups folded, narration and subagent rows shown.
    #[default]
    Steps,
    /// Every group open.
    All,
}

impl Depth {
    /// Every depth, shallow to deep.
    pub const ALL: [Self; 3] = [Self::Prompts, Self::Steps, Self::All];

    /// Its name, as its control and its key's tooltip say it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Prompts => "Prompts",
            Self::Steps => "Steps",
            Self::All => "All",
        }
    }

    /// The depth `delta` steps deeper (negative: shallower), held at the ends.
    #[must_use]
    pub fn step(self, delta: isize) -> Self {
        let at = Self::ALL.iter().position(|depth| *depth == self).unwrap_or(1);
        let to = at.saturating_add_signed(delta).min(Self::ALL.len() - 1);
        Self::ALL[to]
    }
}

/// Of `rows` (in order, `depth_of` saying how deep each is spliced under an opened subagent),
/// what Prompts shows: each prompt, and the last reply of the turn it opened, the run of words
/// its last answer ends on; nothing spliced in under a subagent.
pub(super) fn prompts_only(rows: Vec<Row>, depth_of: impl Fn(&Row) -> usize) -> Vec<Row> {
    let own: Vec<Row> = rows.into_iter().filter(|row| depth_of(row) == 0).collect();
    let mut keep = vec![false; own.len()];
    let mut start = 0;
    while start < own.len() {
        let end = own[start + 1..]
            .iter()
            .position(|row| matches!(row.kind, RowKind::User { .. }))
            .map_or(own.len(), |at| start + 1 + at);
        if matches!(own[start].kind, RowKind::User { .. }) {
            keep[start] = true;
        }
        // The turn's last words, back from its last one to the first row that is not its words.
        let words =
            |row: &Row| matches!(row.kind, RowKind::Markdown { .. } | RowKind::LiveMarkdown { .. });
        if let Some(last) = (start..end).rev().find(|ix| words(&own[*ix])) {
            let entry = own[last].entry_id.clone();
            for ix in (start..=last).rev() {
                if !words(&own[ix]) || own[ix].entry_id != entry {
                    break;
                }
                keep[ix] = true;
            }
        }
        start = end;
    }
    own.into_iter().zip(keep).filter_map(|(row, keep)| keep.then_some(row)).collect()
}

/// Where the keyboard's cursor stands: a row, or a step of an open group by its part id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub row: SharedString,
    pub step: Option<String>,
}

impl Cursor {
    fn row(row: &SharedString) -> Self {
        Self { row: row.clone(), step: None }
    }
}

/// Whether step `tool` ended badly: for the key to the next failure, and find's filter.
pub(super) fn failed(tool: &ToolItem) -> bool {
    super::step_failed(tool)
        || (tool.subagent_ref.is_some()
            && matches!(tool.subagent_status, Some(slopty_view_doc::SubagentStatus::Failed)))
}

impl Transcript {
    /// Whether the trace itself holds the keyboard, so its keys reach it.
    pub(crate) fn holds_keys(&self, window: &Window) -> bool {
        self.keys.is_focused(window)
    }

    /// The trace's place for the keyboard.
    pub(crate) fn keys_handle(&self) -> gpui::FocusHandle {
        self.keys.clone()
    }

    /// Whether group row `row` shows its steps as rows of their own: it folds more than one step
    /// and is open.
    fn group_shows_steps(&self, row: &Row) -> bool {
        let RowKind::ToolGroup { tools, .. } = &row.kind else { return false };
        let (collapses, solo) = super::group_shape(tools);
        collapses && !solo && self.steps_shown(self.folds.get(&row.id).and_then(|fold| fold.open))
    }

    /// The places ↑ and ↓ move over, in order: every row shown, and each step of an open group
    /// under its line.
    fn stops(&self) -> Vec<Cursor> {
        let mut stops = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            stops.push(Cursor::row(&row.id));
            if self.group_shows_steps(row)
                && let RowKind::ToolGroup { tools, .. } = &row.kind
            {
                stops.extend(
                    tools.iter().map(|tool| Cursor {
                        row: row.id.clone(),
                        step: Some(tool.part_id.clone()),
                    }),
                );
            }
        }
        stops
    }

    /// Put the cursor on `cursor` and scroll only as far as its row needs to be seen.
    fn land(&mut self, cursor: Cursor) {
        if let Some(ix) = self.rows.iter().position(|row| row.id == cursor.row) {
            self.reveal_row(ix);
        }
        self.cursor = Some(cursor);
    }

    /// Scroll only as far as row `ix` needs to be seen, letting go of the end unless it is the
    /// last row.
    fn reveal_row(&mut self, ix: usize) {
        if ix + 1 < self.rows.len() {
            self.begin_scroll_navigation();
        }
        self.list.scroll_to_reveal_item(ix);
    }

    /// Every row of the trace, what the depth hides included: what find and the key to the next
    /// failure look through.
    pub(super) fn whole_rows(&self) -> &[Row] {
        self.whole.as_deref().unwrap_or(&self.rows)
    }

    /// Open what hides `target`: the depth, its group, and its detail when `detail`; put the
    /// cursor on it and say its row's place, for the caller to bring it into view.
    pub(super) fn open_to(
        &mut self,
        target: Cursor,
        detail: bool,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        // A row the depth hides shows at Steps.
        if self.depth == Depth::Prompts && !self.rows.iter().any(|row| row.id == target.row) {
            self.set_depth(Depth::Steps, cx);
        }
        let ix = self.rows.iter().position(|row| row.id == target.row)?;
        let shape = match (&self.rows[ix].kind, &target.step) {
            (RowKind::ToolGroup { tools, .. }, Some(step)) => Some((
                super::group_shape(tools),
                tools.iter().position(|tool| &tool.part_id == step),
            )),
            _ => None,
        };
        let mut cursor = Cursor::row(&target.row);
        if let Some(((collapses, solo), at)) = shape {
            // A run that folds shows its steps as rows of their own, the cursor on one.
            if collapses && !solo {
                if !self.group_shows_steps(&self.rows[ix]) {
                    let fold = self.folds.entry(target.row.clone()).or_default();
                    fold.open = Some(true);
                    fold.epoch += 1;
                }
                cursor.step = target.step;
            }
            if let Some(at) = at.filter(|_| detail) {
                let fold = self
                    .tool_details
                    .entry(SharedString::from(format!("{}#d{at}", target.row)))
                    .or_default();
                if fold.open != Some(true) {
                    fold.open = Some(true);
                    fold.epoch += 1;
                    fold.toggled_at = None;
                }
            }
            self.row_resized(&target.row, cx);
        }
        self.cursor = Some(cursor);
        Some(ix)
    }

    /// Move the cursor `delta` places over the stops; from none, start at the first row in view.
    fn move_cursor(&mut self, delta: isize) {
        let stops = self.stops();
        if stops.is_empty() {
            return;
        }
        let at =
            self.cursor.as_ref().and_then(|cursor| stops.iter().position(|stop| stop == cursor));
        let to = match at {
            Some(at) => at.saturating_add_signed(delta).min(stops.len() - 1),
            None => {
                let top =
                    self.list.logical_scroll_top().item_ix.min(self.rows.len().saturating_sub(1));
                let first = self.rows.get(top).map(|row| row.id.clone());
                stops.iter().position(|stop| Some(&stop.row) == first.as_ref()).unwrap_or(0)
            }
        };
        self.land(stops[to].clone());
    }

    /// The row under the cursor and the index of its step in its group, when it stands on one.
    fn under_cursor(&self) -> Option<(Row, Option<usize>)> {
        let cursor = self.cursor.as_ref()?;
        let row = self.rows.iter().find(|row| row.id == cursor.row)?.clone();
        let step = match (&row.kind, &cursor.step) {
            (RowKind::ToolGroup { tools, .. }, Some(step)) => {
                tools.iter().position(|tool| &tool.part_id == step)
            }
            _ => None,
        };
        Some((row, step))
    }

    /// Open (`open`) or fold what the cursor stands on: a group's line, a step's detail, a
    /// subagent opened in place. Folding a step that is shut goes up to its group's line.
    fn fold_at_cursor(&mut self, open: bool, cx: &mut Context<Self>) {
        let Some((row, step)) = self.under_cursor() else { return };
        match (&row.kind, step) {
            (RowKind::ToolGroup { tools, .. }, step) => {
                let (collapses, solo) = super::group_shape(tools);
                // A lone call, or a run that does not fold, is its own step.
                let step = step.or((!collapses || solo).then_some(0));
                match step {
                    Some(ix) => {
                        let key = SharedString::from(format!("{}#d{ix}", row.id));
                        let shown =
                            self.tool_details.get(&key).and_then(|fold| fold.open).unwrap_or(false);
                        if shown != open {
                            let fold = self.tool_details.entry(key).or_default();
                            fold.open = Some(open);
                            fold.epoch += 1;
                            fold.toggled_at = None;
                            if open {
                                self.reveal_opened(row.id.clone());
                            }
                        } else if !open && collapses && !solo {
                            self.cursor = Some(Cursor::row(&row.id));
                        }
                    }
                    None if self.group_shows_steps(&row) != open => {
                        self.toggle_fold(row.id.clone(), 0.0, cx)
                    }
                    None => {}
                }
            }
            (RowKind::Spawn { tool }, _) => {
                if let Some(chat) = super::subagents::Run::of(tool).chat
                    && self.spawn_open(&row.id) != open
                {
                    self.toggle_spawn(row.id.clone(), chat, cx);
                }
            }
            _ if !open && self.depth_of(&row.id) > 0 => {
                // Inside a subagent opened in place, ← goes up to its row.
                let ix = self.rows.iter().position(|r| r.id == row.id).unwrap_or(0);
                let depth = self.depth_of(&row.id);
                if let Some(up) =
                    self.rows[..ix].iter().rev().find(|r| self.depth_of(&r.id) < depth)
                {
                    self.cursor = Some(Cursor::row(&up.id));
                }
            }
            _ => {}
        }
        self.row_resized(&row.id, cx);
    }

    /// Enter on the cursor: a subagent drills in; anything else that opens opens or folds.
    fn enter_at_cursor(&mut self, cx: &mut Context<Self>) {
        let Some((row, step)) = self.under_cursor() else { return };
        if let RowKind::Spawn { tool } = &row.kind
            && let Some(chat) = super::subagents::Run::of(tool).chat
        {
            cx.emit(super::SubagentDrill { chat_id: chat, row: row.id.clone() });
            return;
        }
        let open = match (&row.kind, step) {
            (RowKind::ToolGroup { tools, .. }, step) => {
                let (collapses, solo) = super::group_shape(tools);
                match step.or((!collapses || solo).then_some(0)) {
                    Some(ix) => !self
                        .tool_details
                        .get(&SharedString::from(format!("{}#d{ix}", row.id)))
                        .and_then(|fold| fold.open)
                        .unwrap_or(false),
                    None => !self.group_shows_steps(&row),
                }
            }
            _ => return,
        };
        self.fold_at_cursor(open, cx);
    }

    /// Move the cursor to the next failed step (`delta` 1) or the one before (-1) in the whole
    /// trace, opening what hides it so it lands on a row of its own.
    fn next_failure(&mut self, delta: isize, cx: &mut Context<Self>) {
        // Where a place stands in the trace's order: its row, then its step in a run that folds
        // (a lone call is its row).
        let order = |rows: &[Row], cursor: &Cursor| -> Option<(usize, usize)> {
            let ix = rows.iter().position(|row| row.id == cursor.row)?;
            let step = match (&rows[ix].kind, &cursor.step) {
                (RowKind::ToolGroup { tools, .. }, Some(step)) => {
                    let (collapses, solo) = super::group_shape(tools);
                    let at = tools.iter().position(|tool| &tool.part_id == step);
                    at.filter(|_| collapses && !solo).map_or(0, |at| at + 1)
                }
                _ => 0,
            };
            Some((ix, step))
        };
        let rows = self.whole_rows();
        // Every failure in order: a failed subagent's row, a failed step of a group, wherever
        // its group is folded or the depth hides it.
        let mut failures: Vec<((usize, usize), Cursor)> = Vec::new();
        for row in rows {
            let targets: Vec<Cursor> = match &row.kind {
                RowKind::Spawn { tool } if failed(tool) => vec![Cursor::row(&row.id)],
                RowKind::ToolGroup { tools, .. } => tools
                    .iter()
                    .filter(|tool| failed(tool))
                    .map(|tool| Cursor { row: row.id.clone(), step: Some(tool.part_id.clone()) })
                    .collect(),
                _ => Vec::new(),
            };
            failures.extend(
                targets.into_iter().filter_map(|target| Some((order(rows, &target)?, target))),
            );
        }
        let here = self.cursor.as_ref().and_then(|cursor| order(rows, cursor));
        let target = if delta >= 0 {
            failures.iter().find(|(at, _)| here.is_none_or(|here| *at > here)).or(failures.first())
        } else {
            failures
                .iter()
                .rev()
                .find(|(at, _)| here.is_none_or(|here| *at < here))
                .or(failures.last())
        }
        .map(|(_, target)| target.clone());
        let Some(target) = target else { return };
        if let Some(ix) = self.open_to(target, false, cx) {
            self.reveal_row(ix);
        }
    }

    /// The trace's own keys, where it holds the keyboard: ↑ ↓ move, ← → fold and open, Enter
    /// opens or drills, E and ⇧E go to the next and the previous failure.
    pub(super) fn trace_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.keys.is_focused(window) {
            return;
        }
        let modifiers = event.keystroke.modifiers;
        let shift_only =
            modifiers.shift && !modifiers.control && !modifiers.alt && !modifiers.platform;
        match (event.keystroke.key.as_str(), modifiers.modified()) {
            ("up", false) => self.move_cursor(-1),
            ("down", false) => self.move_cursor(1),
            ("left", false) => self.fold_at_cursor(false, cx),
            ("right", false) => self.fold_at_cursor(true, cx),
            ("enter", false) => self.enter_at_cursor(cx),
            ("e", false) => self.next_failure(1, cx),
            ("e", true) if shift_only => self.next_failure(-1, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// How much of the trace shows.
    pub(crate) fn depth(&self) -> Depth {
        self.depth
    }

    /// Show the trace at `depth`; the folds the person pressed keep their way.
    pub(crate) fn set_depth(&mut self, depth: Depth, cx: &mut Context<Self>) {
        if self.depth == depth {
            return;
        }
        self.depth = depth;
        self.last_source = None;
        // A group's height follows its fold, which a depth changes for every unpressed group.
        let groups: Vec<_> = self
            .rows
            .iter()
            .filter(|row| matches!(row.kind, RowKind::ToolGroup { .. }))
            .map(|row| row.id.clone())
            .collect();
        for group in groups {
            self.row_resized(&group, cx);
        }
        self.sync(cx);
        cx.notify();
    }

    /// Whether a group whose fold is `open` (none: never pressed) shows its steps: as the person
    /// left it, else as the depth has it.
    pub(super) fn steps_shown(&self, open: Option<bool>) -> bool {
        open.unwrap_or(self.depth == Depth::All)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::SharedString;

    use super::*;

    fn row(id: &str, entry: &str, kind: RowKind) -> Row {
        Row {
            id: SharedString::from(id.to_owned()),
            version: 0,
            turn_start: false,
            kind,
            entry_id: SharedString::from(entry.to_owned()),
            timestamp: None,
            copy_text: None,
            compact_fold: None,
        }
    }

    fn prompt(id: &str) -> Row {
        row(
            id,
            id,
            RowKind::User {
                text: "Do it".into(),
                markdown: None,
                attachments: Arc::default(),
                turn: None,
            },
        )
    }

    fn words(id: &str, entry: &str) -> Row {
        row(
            id,
            entry,
            RowKind::Markdown { tree: Arc::new(crate::markdown::parse_full("Done.")), block_ix: 0 },
        )
    }

    fn group(id: &str, entry: &str) -> Row {
        row(
            id,
            entry,
            RowKind::ToolGroup { summary: "Ran 2 commands".into(), tools: Arc::default() },
        )
    }

    /// Prompts shows each prompt and its turn's last words: not the work, not the words said
    /// before the last call, nothing under a subagent; a turn with no words shows its prompt.
    #[test]
    fn prompts_shows_each_turn_and_its_last_reply() {
        let rows = vec![
            prompt("p1"),
            words("a1:0", "a1"),
            group("a1#g", "a1"),
            words("a1:1", "a1"),
            words("a1:2", "a1"),
            prompt("p2"),
            words("a2:0", "a2"),
            group("a2#g", "a2"),
            words("nested", "child"),
            prompt("p3"),
        ];
        let kept: Vec<String> = prompts_only(rows, |row| usize::from(row.id == "nested"))
            .iter()
            .map(|row| row.id.to_string())
            .collect();
        assert_eq!(kept, ["p1", "a1:1", "a1:2", "p2", "a2:0", "p3"]);
    }

    /// A depth steps deeper and shallower, and holds at its ends.
    #[test]
    fn a_depth_steps_and_holds_at_its_ends() {
        assert_eq!(Depth::Steps.step(1), Depth::All);
        assert_eq!(Depth::All.step(1), Depth::All);
        assert_eq!(Depth::Steps.step(-1), Depth::Prompts);
        assert_eq!(Depth::Prompts.step(-1), Depth::Prompts);
    }
}
