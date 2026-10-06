//! A tile in the overview's small zoom: a map of its work, not its pixels scaled.
//!
//! Text scaled a few points high is texture, not words, so a tile of words (a shell, a
//! conversation, a file, a folder, a review) is summed up at chrome size once the overview has
//! landed: what it is (its kind or its agent's mark) and its title at the task-title size, how
//! it stands and on which machine, and at most two facts (what it is doing or where, and its
//! working tree's changes). A glance then says which tile deserves the person before it is
//! opened (`docs/decisions/workspace.md`, "The overview is a map of work").
//!
//! While the zoom moves the tile's own body is drawn at the zoom, so the tile is seen shrinking
//! into place. Once the overview rests, a body under its summary is not drawn at all
//! ([`WorkspaceView::summed_up`]), so output nobody can read there costs no frame; the focused
//! one is, as it keeps the keyboard. A tile a glance knows by sight (a remote window or display,
//! a page, a picture, a PDF, a film) keeps its live picture, named by a label at its foot.
//! With the overview closed there is no miniature at all.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use slopty_client::layout::{Placed, TileRef};
use slopty_core::{ItemId, SessionId};
use slopty_proto::items::{Item, ItemKind};
use slopty_proto::terminal::SessionState;
use slopty_proto::thread::ThreadId;
use slopty_theme::Typography;

use super::WorkspaceView;
use super::rollup::meta_line;
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{FileType, Status};

/// Whether `item`'s miniature is its picture: what a glance knows by sight. Every other
/// tile's miniature is a summary of its work.
fn pictured(item: &Item) -> bool {
    match &item.kind {
        ItemKind::Window { .. } | ItemKind::Display { .. } | ItemKind::Browser { .. } => true,
        ItemKind::File { path } => {
            matches!(FileType::of(path), Some(FileType::Image | FileType::Pdf | FileType::Video))
        }
        ItemKind::Terminal { .. }
        | ItemKind::Thread { .. }
        | ItemKind::Folder { .. }
        | ItemKind::Review { .. }
        | ItemKind::Changes { .. } => false,
    }
}

impl WorkspaceView {
    /// Whether `item`'s miniature is wholly its summary now: the overview has landed and holds
    /// still, the tile is one of words, and it does not hold the keyboard.
    pub(super) fn summed_up(&self, placed: &Placed, item: &Item) -> bool {
        let rests = self.layout.overview_open() && !self.layout.is_animating();
        rests && !placed.focused && !pictured(item)
    }

    /// The body `content` as its tile's miniature: a picture with its label at its foot, or a
    /// summary of its work laid over it.
    pub(super) fn render_miniature(
        &self,
        placed: &Placed,
        item: &Item,
        content: gpui::AnyElement,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let id = item.id;
        let over = if pictured(item) {
            self.miniature_label(placed, item, cx)
        } else {
            self.miniature_summary(placed, item, cx)
        };
        div()
            .debug_selector(move || format!("miniature-{}", id.as_uuid()))
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .flex()
            .flex_col()
            .child(content)
            .children(over)
            .into_any_element()
    }

    /// The name row of a miniature: what the tile is (its kind or its agent's mark), its title,
    /// and how it stands at the line's end, at `size` points.
    fn miniature_name(&self, placed: &Placed, item: &Item, size: f32) -> gpui::Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let id = item.id;
        let (mark, _) = self.tile_marks(placed.tile, item);
        let lead =
            crate::palette::lead_slot(theme, self.kind_glyph(item), hsla(s.text_secondary), 1.0);
        let state = mark
            .filter(|m| *m != Status::Idle)
            .map(|m| crate::icons::status_mark(theme, Some(m), 1.0));
        div()
            .debug_selector(move || format!("shapes-label-{}", id.as_uuid()))
            .h(px(theme.typography.icon_large()))
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .overflow_hidden()
            .child(lead)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(size))
                    .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                    .text_color(hsla(s.text))
                    .child(SharedString::from(self.tile_title(item))),
            )
            .children(state)
    }

    /// A tile of words summed up over its body once the overview has landed, on the block's
    /// ground: its name row at the task-title size; three lines of facts, each always saying
    /// something (how it stands, what it did last, where it is); then a few lines of its own
    /// text in its body's face, clipped to the card, the text's end at its foot where the text
    /// grows at its end.
    fn miniature_summary(
        &self,
        placed: &Placed,
        item: &Item,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let t = &theme.typography;
        let id = item.id;
        let (mark, _) = self.tile_marks(placed.tile, item);
        let digest = self.facts.digest(id);
        let lines = self.summary_lines(placed.tile, item, mark, digest, cx);
        let line = |name: &'static str, words: String, tone| {
            div()
                .debug_selector(move || format!("{name}-{}", id.as_uuid()))
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(hsla(tone))
                .child(SharedString::from(words))
        };
        let standing = line("shapes-standing", lines.standing, s.text);
        let doing = line("shapes-meta", lines.doing, s.text_muted);
        let changes =
            lines.changes.and_then(|(added, removed)| crate::kit::changes(theme, added, removed));
        let place = div()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .min_w_0()
            .child(line("shapes-place", lines.place, s.text_muted))
            .children(changes.map(|c| c.flex_none().text_size(px(t.small()))));
        let facts = div()
            .flex()
            .flex_col()
            .min_w_0()
            .text_size(px(t.small()))
            .line_height(px(super::navigator::line_heights(theme).1))
            .child(standing)
            .child(doing)
            .child(place);
        let tail = digest.filter(|d| !d.tail.is_empty()).map(|d| self.miniature_tail(id, d));
        // On the block's own ground, no fill and no outline of its own: the tiles are parted
        // by the space their words keep from their edges, so the block holds work, not boxes.
        let card = div()
            .id(SharedString::from(format!("miniature-summary-{}", id.as_uuid())))
            .absolute()
            .inset_0()
            .p(px(theme.spacing.md))
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xs))
            .overflow_hidden()
            .font_family(t.ui_family.clone())
            .child(self.miniature_name(placed, item, theme.roles().task_title.size))
            .child(facts)
            .children(tail);
        let words = SharedString::from(format!("miniature-in-{}", id.as_uuid()));
        super::strip::overview_words(
            div()
                .debug_selector(move || format!("miniature-label-{}", id.as_uuid()))
                .absolute()
                .inset_0()
                .bg(hsla(theme.content()))
                .child(card),
            words,
            self.layout.overview_open(),
            self.chrome_moves(cx),
        )
    }

    /// A summary's own text: the lines its digest copied, in the body's face, quiet, a base
    /// unit under the facts, filling what the card has left and clipped to it. A file's first lines
    /// hang from the top, as do a few rows of a shell, as a fresh screen's do; more of a
    /// shell's rows or an agent's words stand on the card's foot, so the newest is the one
    /// never clipped.
    fn miniature_tail(&self, id: ItemId, digest: &Digest) -> gpui::Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let t = &theme.typography;
        let size = t.caption();
        div()
            .debug_selector(move || format!("shapes-tail-{}", id.as_uuid()))
            .flex_1()
            .min_h_0()
            .mt(px(theme.spacing.sm))
            .flex()
            .flex_col()
            .when(digest.from_end && digest.tail.len() > TAIL_HANGS, gpui::Styled::justify_end)
            .overflow_hidden()
            .font_family(crate::palette::mono_family(theme))
            .text_size(px(size))
            .line_height(px(size * TAIL_LINE))
            .text_color(hsla(s.text_muted))
            .children(digest.tail.iter().map(|row| {
                div()
                    .flex_none()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(row.clone())
            }))
    }

    /// What `tile`'s summary says, as words: its three fact lines, then its tail.
    #[cfg(test)]
    pub(super) fn summary_words(&self, tile: TileRef, cx: &App) -> (Vec<String>, Vec<String>) {
        let Some(item) = self.item(tile) else { return (Vec::new(), Vec::new()) };
        let (mark, _) = self.tile_marks(tile, item);
        let digest = self.facts.digest(item.id);
        let lines = self.summary_lines(tile, item, mark, digest, cx);
        let tail = digest.map(|d| d.tail.iter().map(ToString::to_string).collect());
        (vec![lines.standing, lines.doing, lines.place], tail.unwrap_or_default())
    }

    /// The three fact lines of `item`'s summary on `tile`, `mark` being its status: each says
    /// something, a quieter fact standing in where the first choice has nothing to say.
    fn summary_lines(
        &self,
        tile: TileRef,
        item: &Item,
        mark: Option<Status>,
        digest: Option<&Digest>,
        cx: &App,
    ) -> SummaryLines {
        let worker = self.worker_name(tile.worker);
        let several = self.workers.len() > 1;
        // Where it is, with its machine where there are several; its machine alone else.
        let place = |at: Option<&str>| {
            let line = meta_line([at, several.then_some(worker.as_str())]);
            if line.is_empty() { worker.clone() } else { line }
        };
        match &item.kind {
            ItemKind::Terminal { session } if self.agent_state(*session).is_some() => {
                let (doing, _) = self.shell_doing(*session);
                let summary = self.summary(*session);
                let at = self.session_tail(*session);
                let branch = summary.and_then(|s| s.branch.as_deref());
                SummaryLines {
                    standing: self.tile_word(item, mark).unwrap_or_else(|| AT_REST.to_owned()),
                    doing: doing.unwrap_or_else(|| self.agent_name(item)),
                    place: place(Some(&meta_line([at.as_deref(), branch]))),
                    changes: self.session_changes(*session),
                }
            }
            ItemKind::Terminal { session } => self.shell_lines(*session, place),
            ItemKind::Thread { thread } => {
                let stand = self.thread_stand(*thread);
                let doing = stand
                    .and_then(|s| s.doing.clone())
                    .or_else(|| self.thread_line(*thread).map(crate::markdown::plain_line));
                SummaryLines {
                    standing: stand
                        .and_then(super::faces::ThreadStand::word)
                        .unwrap_or(AT_REST)
                        .to_owned(),
                    doing: doing.unwrap_or_else(|| self.agent_name(item)),
                    place: place(self.tile_place(item).as_deref()),
                    changes: None,
                }
            }
            ItemKind::File { path } => self.file_lines(item.id, path, digest, place),
            ItemKind::Folder { .. } | ItemKind::Review { .. } | ItemKind::Changes { .. } => {
                let (meta, _) = self.tile_meta(item, std::time::SystemTime::now(), cx);
                SummaryLines {
                    standing: self
                        .tile_word(item, mark)
                        .unwrap_or_else(|| kind_word(item).to_owned()),
                    doing: if meta.is_empty() { worker.clone() } else { meta },
                    place: place(self.tile_place(item).as_deref()),
                    changes: None,
                }
            }
            ItemKind::Window { .. } | ItemKind::Display { .. } | ItemKind::Browser { .. } => {
                SummaryLines {
                    standing: kind_word(item).to_owned(),
                    doing: worker.clone(),
                    place: place(None),
                    changes: None,
                }
            }
        }
    }

    /// A shell's facts: how it stands (at its prompt, running a command with its clock, or
    /// exited), the command it ran last and how that ended, and where it is.
    fn shell_lines(
        &self,
        session: SessionId,
        place: impl Fn(Option<&str>) -> String,
    ) -> SummaryLines {
        let summary = self.summary(session);
        let shell = self.shell(session);
        let exited = summary.and_then(|s| match s.state {
            SessionState::Exited { status } => Some(status),
            SessionState::Running => None,
        });
        let program = summary.and_then(|s| s.command.first()).map(|c| {
            let name = c.rsplit('/').next().unwrap_or(c);
            name.to_owned()
        });
        let running = shell.and_then(|s| s.running.as_deref()).map(super::tile::command_words);
        let standing = match (exited, running) {
            (Some(0), _) => "Exited".to_owned(),
            (Some(code), _) => format!("Exited {code}"),
            (None, Some(command)) => {
                let clock = self.running_for(session).map(crate::kit::duration);
                meta_line([Some(format!("Running {command}").as_str()), clock.as_deref()])
            }
            (None, None) if shell.is_some_and(|s| s.prompted) => "At the prompt".to_owned(),
            (None, None) => {
                program.as_deref().map_or_else(|| "Starting".to_owned(), |p| format!("Running {p}"))
            }
        };
        let last = shell.and_then(|s| {
            let command = super::tile::command_words(s.last.as_deref()?);
            let ended = match s.exit {
                Some(0) | None => "Done".to_owned(),
                Some(code) => format!("Exit {code}"),
            };
            (!command.is_empty()).then(|| meta_line([Some(command), Some(ended.as_str())]))
        });
        let doing = last.unwrap_or_else(|| match summary {
            _ if shell.is_some_and(|s| s.prompted) => "No commands yet".to_owned(),
            Some(s) if s.command.len() > 1 => s.command.join(" "),
            Some(s) => format!("{} \u{d7} {}", s.cols, s.rows),
            None => "No commands yet".to_owned(),
        });
        let at = self.session_tail(session);
        let branch = summary.and_then(|s| s.branch.as_deref());
        SummaryLines {
            standing,
            doing,
            place: place(Some(&meta_line([at.as_deref(), branch]))),
            changes: self.session_changes(session),
        }
    }

    /// A file's facts: its kind and length, then its task list's progress or whether its edit
    /// is on disk, then its folder.
    fn file_lines(
        &self,
        id: ItemId,
        path: &str,
        digest: Option<&Digest>,
        place: impl Fn(Option<&str>) -> String,
    ) -> SummaryLines {
        let facts = self.file_facts(id);
        let kind = digest.and_then(|d| d.kind).unwrap_or_else(|| match FileType::of(path) {
            Some(FileType::Code) => "Code",
            Some(FileType::Data) => "Data",
            _ => "Text",
        });
        let length = digest
            .and_then(|d| d.lines)
            .map(|n| if n == 1 { "1 line".to_owned() } else { format!("{n} lines") });
        let standing = if facts.has_text {
            meta_line([Some(kind), length.as_deref()])
        } else {
            READING.to_owned()
        };
        let tasks = digest.and_then(|d| d.tasks).map(|(done, all)| format!("{done} of {all} done"));
        let state = if facts.conflict {
            "Changed on disk"
        } else if facts.unsaved {
            "Edited"
        } else if digest.is_some_and(|d| d.read_only) {
            "Read only"
        } else {
            "Saved"
        };
        // Progress says more than a file at rest: "Saved" yields to it.
        let state = (facts.conflict || facts.unsaved || tasks.is_none()).then_some(state);
        SummaryLines {
            standing,
            doing: meta_line([tasks.as_deref(), state]),
            place: place(self.tile_place_of(id).as_deref()),
            changes: None,
        }
    }

    /// The agent that runs `item`, by name: what an agent with nothing yet to say says.
    fn agent_name(&self, item: &Item) -> String {
        self.item_agent(item).map_or_else(
            || AT_REST.to_owned(),
            |agent| super::projects::agent_label(&slopty_proto::thread::AgentId::named(agent)),
        )
    }

    /// What `session`'s working tree has changed, while it is in one and has.
    fn session_changes(&self, session: SessionId) -> Option<(u32, u32)> {
        self.summary(session)
            .and_then(|summary| summary.changes)
            .and_then(super::navigator::line_changes)
    }

    /// Item `id`'s place, as its tile says it.
    fn tile_place_of(&self, id: ItemId) -> Option<String> {
        let tile = self.tile_of(id)?;
        self.tile_place(self.item(tile)?)
    }

    /// Copy every tile of words' digest: the overview has just opened.
    fn take_digests(&mut self, cx: &App) {
        let items: Vec<Item> =
            self.items().map(|(_, item)| item).filter(|i| !pictured(i)).cloned().collect();
        let digests = items.iter().map(|item| (item.id, self.digest(item, cx))).collect();
        self.facts.set_digests(digests);
    }

    /// The overview opened or closed: opened, it copies what each tile of words holds.
    pub(super) fn overview_flipped(&mut self, cx: &App) {
        if self.layout.overview_open() {
            self.take_digests(cx);
        } else {
            self.facts.set_digests(std::collections::HashMap::new());
        }
    }

    /// Close the overview, and let go of what it copied.
    pub(super) fn close_overview(&mut self) {
        self.layout.set_overview(false);
        self.facts.set_digests(std::collections::HashMap::new());
    }

    /// Copy item `id`'s digest again while the overview shows: its body's facts changed.
    /// Whether the copy differs from the last.
    pub(super) fn retake_digest(&mut self, id: ItemId, cx: &App) -> bool {
        if !self.layout.overview_open() {
            return false;
        }
        let Some(item) = self.tile_of(id).and_then(|t| self.item(t)).cloned() else {
            return false;
        };
        if pictured(&item) {
            return false;
        }
        let digest = self.digest(&item, cx);
        if self.facts.digest(id) == Some(&digest) {
            return false;
        }
        self.facts.put_digest(id, digest);
        true
    }

    /// [`Self::retake_digest`] for the tile of `session`.
    pub(super) fn retake_shell_digest(&mut self, session: SessionId, cx: &App) -> bool {
        if !self.layout.overview_open() {
            return false;
        }
        let id = self.items().find_map(|(_, item)| match item.kind {
            ItemKind::Terminal { session: s } if s == session => Some(item.id),
            _ => None,
        });
        id.is_some_and(|id| self.retake_digest(id, cx))
    }

    /// Copy again the digests of the tiles whose agents speak: their newest words came.
    pub(super) fn retake_agent_digests(&mut self, cx: &App) {
        if !self.layout.overview_open() {
            return;
        }
        let agents: Vec<ItemId> = self
            .items()
            .filter(|(_, item)| match item.kind {
                ItemKind::Thread { .. } => true,
                ItemKind::Terminal { session } => self.agent_state(session).is_some(),
                _ => false,
            })
            .map(|(_, item)| item.id)
            .collect();
        for id in agents {
            let _changed = self.retake_digest(id, cx);
        }
    }

    /// What `item`'s summary shows of its body, read out of it now.
    fn digest(&self, item: &Item, cx: &App) -> Digest {
        let words = |thread: Option<ThreadId>| {
            let text = thread.and_then(|t| self.last_words(t, cx)).unwrap_or_default();
            let lines: Vec<String> = text
                .lines()
                .map(crate::markdown::plain_line)
                .filter(|l| !l.trim().is_empty())
                .collect();
            Digest { tail: end_of(lines), from_end: true, ..Digest::default() }
        };
        match &item.kind {
            ItemKind::Terminal { session } if self.agent_state(*session).is_some() => {
                words(self.session_thread(*session))
            }
            ItemKind::Terminal { session } => {
                self.terminals.get(session).map_or_else(Digest::default, |view| {
                    let rows = view.read(cx).rows();
                    let used = rows
                        .iter()
                        .rposition(|r| !r.trim().is_empty())
                        .map_or(0, |at| at.saturating_add(1));
                    let rows = rows.into_iter().take(used).collect();
                    Digest { tail: end_of(rows), from_end: true, ..Digest::default() }
                })
            }
            ItemKind::Thread { thread } => words(Some(*thread)),
            ItemKind::File { .. } => {
                self.files.get(&item.id).map_or_else(Digest::default, |view| {
                    let view = view.read(cx);
                    if !view.shows_text() {
                        return Digest::default();
                    }
                    let text = view.text(cx);
                    let kind = view.coloured_as();
                    let markdown = kind == Some("Markdown");
                    let tasks = markdown.then(|| tasks_of(&text)).flatten();
                    let head: Vec<String> =
                        text.lines().take(TAIL_LINES).map(str::to_owned).collect();
                    let used = head
                        .iter()
                        .rposition(|l| !l.trim().is_empty())
                        .map_or(0, |at| at.saturating_add(1));
                    Digest {
                        tail: head.into_iter().take(used).map(tail_line).collect(),
                        from_end: false,
                        lines: Some(view.line_count(cx)),
                        tasks,
                        kind,
                        read_only: view.read_only().is_some(),
                    }
                })
            }
            ItemKind::Folder { .. }
            | ItemKind::Review { .. }
            | ItemKind::Changes { .. }
            | ItemKind::Window { .. }
            | ItemKind::Display { .. }
            | ItemKind::Browser { .. } => Digest::default(),
        }
    }

    /// The thin label under a pictured miniature: its name row where the workspace's name
    /// above the block starts, then the tile's facts in the meta size. Drawn once the zoom has
    /// all but landed, and not while the overview closes, as the overview's other words are.
    fn miniature_label(
        &self,
        placed: &Placed,
        item: &Item,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let t = &theme.typography;
        let id = item.id;
        let muted = hsla(s.text_muted);
        let name = self.miniature_name(placed, item, t.small());
        let worker = (self.workers.len() > 1).then(|| self.worker_name(placed.tile.worker));
        let (meta, _) = self.tile_meta(item, std::time::SystemTime::now(), cx);
        let meta = meta_line([Some(meta.as_str()), worker.as_deref()]);
        let meta = (!meta.is_empty()).then(|| {
            div()
                .debug_selector(move || format!("shapes-meta-{}", id.as_uuid()))
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .text_size(px(t.small()))
                .text_color(muted)
                .child(SharedString::from(meta))
        });
        let label = div()
            .debug_selector(move || format!("miniature-label-{}", id.as_uuid()))
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(2.0_f32.mul_add(theme.spacing.xs, t.icon_large())))
            .px(px(theme.spacing.md))
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .overflow_hidden()
            .whitespace_nowrap()
            .font_family(t.ui_family.clone())
            .text_size(px(t.small()))
            .bg(hsla(theme.content()))
            .child(name.flex_none().max_w_2_3())
            .children(meta);
        let words = SharedString::from(format!("miniature-in-{}", id.as_uuid()));
        super::strip::overview_words(
            label,
            words,
            self.layout.overview_open(),
            self.chrome_moves(cx),
        )
    }
}

/// How many lines of its text a summary keeps: what a card shows at the overview's usual zoom.
const TAIL_LINES: usize = 6;
/// How many characters of each: more than a card is wide.
const TAIL_CHARS: usize = 120;
/// The most lines of a text's end that hang from the top, as a fresh screen's do: fewer than
/// the smallest card shows, so none is clipped.
const TAIL_HANGS: usize = 3;
/// A tail's line height, over its size: set close, as a terminal's rows are.
const TAIL_LINE: f32 = 1.4;
/// What a tile says while nothing is under way.
const AT_REST: &str = "Idle";
/// What a file says before its text is in.
const READING: &str = "Reading\u{2026}";

/// What a summed-up tile shows of its body beside the facts the workspace keeps: a few lines
/// of its text, and what only the text says. Copied out of the body when the overview opens
/// and again when the body's facts change while it shows (a command starts or ends, a file's
/// edit is saved, an agent's table row moves), never while drawing: output under a resting
/// overview costs no frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Digest {
    /// A few lines of its text: a shell's last rows or an agent's last words, the newest
    /// last; a file's first lines.
    pub tail: Vec<SharedString>,
    /// The tail is the text's end, which stands on the card's foot; else its start.
    pub from_end: bool,
    /// A file's length in lines.
    pub lines: Option<usize>,
    /// A Markdown file's task list: the boxes ticked, and all of them.
    pub tasks: Option<(usize, usize)>,
    /// A file's kind, as its grammar names it ("Markdown").
    pub kind: Option<&'static str>,
    /// A file too large to edit here.
    pub read_only: bool,
}

/// The three fact lines of a summary.
struct SummaryLines {
    /// How it stands, drawn in the text's ink: the card reads state first.
    standing: String,
    /// What it did last, or is doing.
    doing: String,
    /// Where it is.
    place: String,
    /// Its working tree's changes, at the place line's end.
    changes: Option<(u32, u32)>,
}

/// The last [`TAIL_LINES`] of `lines`, as a tail shows them.
fn end_of(lines: Vec<String>) -> Vec<SharedString> {
    let skip = lines.len().saturating_sub(TAIL_LINES);
    lines.into_iter().skip(skip).map(tail_line).collect()
}

/// One line of a tail: tabs as spaces, cut at [`TAIL_CHARS`].
fn tail_line(line: impl AsRef<str>) -> SharedString {
    line.as_ref().replace('\t', "    ").chars().take(TAIL_CHARS).collect::<String>().into()
}

/// A Markdown text's task list: the boxes ticked and all of them, when it has one.
fn tasks_of(text: &str) -> Option<(usize, usize)> {
    let (done, all) = text.lines().filter_map(crate::markdown::task_line).fold(
        (0_usize, 0_usize),
        |(done, all), (ticked, _)| {
            (done.saturating_add(usize::from(ticked)), all.saturating_add(1))
        },
    );
    (all > 0).then_some((done, all))
}

/// A tile's kind in a word, for one with nothing else to say how it stands.
const fn kind_word(item: &Item) -> &'static str {
    match item.kind {
        ItemKind::Terminal { .. } => "Terminal",
        ItemKind::Thread { .. } => "Thread",
        ItemKind::File { .. } => "File",
        ItemKind::Folder { .. } => "Folder",
        ItemKind::Review { .. } => "Review",
        ItemKind::Changes { .. } => "Changes",
        ItemKind::Window { .. } => "Window",
        ItemKind::Display { .. } => "Display",
        ItemKind::Browser { .. } => "Page",
    }
}
