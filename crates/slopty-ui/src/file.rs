//! A file card on the canvas: a file on the host, read-only, as the agent left it.
//!
//! The item (`ItemKind::File`) names the path; the text is not in the document. Each client
//! asks the host for it (`ClientMsg::ReadFile`) when the card appears and again when an agent's
//! edit or write lands, and draws what came back: line-numbered mono rows in a `uniform_list`
//! (a 2 000-line file lays out only the rows on screen), or one line saying why there is
//! nothing to draw. A read that follows one (an agent's edit landed, "reload" pressed) tints
//! the lines that changed and scrolls the first of them into view, so the card shows what the
//! agent just did without the human hunting for it.

use std::sync::Arc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, ElementId, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, Task,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use slopty_core::ItemId;
use slopty_proto::file::FileRead;
use slopty_theme::{Theme, alpha};

use crate::colors::{hsla, hsla_alpha};
use crate::highlight::{self, Span, Syntax};
use crate::terminal::{CloseFind, FindNext, FindPrev};

/// How the reading line moves on a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineMove {
    /// By this many lines (negative up).
    Lines(i64),
    /// By this many pages of visible rows (negative up).
    Pages(i64),
    /// To the first line.
    First,
    /// To the last line.
    Last,
}

/// What a file card tells the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileViewEvent {
    /// The find bar closed (Esc, ✕): the keyboard should go back to the canvas.
    FindClosed,
}

impl EventEmitter<FileViewEvent> for FileView {}

/// The open find bar of a file card.
struct FileSearch {
    input: Entity<InputState>,
    /// What the hits are for.
    needle: String,
    /// Lines (indices into the card's lines) holding the needle, in order.
    hits: Vec<usize>,
    /// Index into `hits` of the one the card is on.
    current: Option<usize>,
    _subscription: Subscription,
}

/// The lines holding `needle`, case-insensitive, in order; none for an empty needle.
#[must_use]
pub fn find_hits(lines: &[SharedString], needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    let needle = needle.to_lowercase();
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains(&needle))
        .map(|(ix, _)| ix)
        .collect()
}

/// The view of one file item.
pub struct FileView {
    id: ItemId,
    path: String,
    /// What the host said, `None` until it answers.
    read: Option<FileRead>,
    /// The text's lines, split once when it arrives.
    lines: Vec<SharedString>,
    /// Lines (indices into `lines`) the last read changed against the one before it, in
    /// order; empty on a first read and when nothing moved.
    changed: Vec<usize>,
    /// The line (index into `lines`) the card was opened at: an edit's place in the file.
    focus: Option<usize>,
    zoom: f32,
    /// Inner padding at zoom 1 (the theme's base spacing).
    pad: f32,
    /// Text size at zoom 1 (the theme's small size).
    text_size: f32,
    theme: Theme,
    scroll: UniformListScrollHandle,
    /// The find bar, while open.
    search: Option<FileSearch>,
    /// The grammar the path (or first line) names; none for a file the bundle cannot colour.
    syntax: Option<Syntax>,
    /// One span list per line, once the background parse of the current text lands.
    spans: Option<Arc<[Vec<Span>]>>,
    /// Which text the running parse is for: a read that lands mid-parse drops the old result.
    generation: u64,
    /// The parse in flight, dropped (cancelled) with the card.
    highlighting: Option<Task<()>>,
}

impl std::fmt::Debug for FileView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileView")
            .field("id", &self.id)
            .field("path", &self.path)
            .field("lines", &self.lines.len())
            .field("zoom", &self.zoom)
            .finish_non_exhaustive()
    }
}

impl FileView {
    /// A card for `path`, waiting on the host.
    #[must_use]
    pub fn new(id: ItemId, path: &str, theme: Theme) -> Self {
        Self {
            id,
            path: path.to_owned(),
            read: None,
            lines: Vec::new(),
            changed: Vec::new(),
            focus: None,
            zoom: 1.0,
            pad: 8.0,
            text_size: 12.0,
            theme,
            scroll: UniformListScrollHandle::new(),
            search: None,
            syntax: None,
            spans: None,
            generation: 0,
            highlighting: None,
        }
    }

    /// The grammar's name ("Rust") once the text is coloured; none before the parse lands or
    /// for a file the bundle cannot colour.
    #[must_use]
    pub fn coloured_as(&self) -> Option<&'static str> {
        self.spans.as_ref().and(self.syntax).map(Syntax::name)
    }

    /// Parse `text` on a background thread and take the spans when they land, if this is
    /// still the text they are for.
    fn recolour(&mut self, text: Arc<str>, cx: &Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.spans = None;
        let first = text.split('\n').next().unwrap_or_default();
        self.syntax = Syntax::for_path(&self.path, first);
        let Some(syntax) = self.syntax else {
            self.highlighting = None;
            return;
        };
        let generation = self.generation;
        let parsing = cx.background_spawn(async move { highlight::spans(&text, syntax) });
        self.highlighting = Some(cx.spawn(async move |this, cx| {
            let spans = parsing.await;
            let _gone = this.update(cx, |view, cx| {
                if view.generation == generation {
                    view.spans = Some(spans.into());
                    cx.notify();
                }
            });
        }));
    }

    /// ↑/↓, ⇞/⇟, Home/End with the card active: move the reading line (the tinted one an
    /// edit opened the card at, which "edit" opens the editor on) and keep it in view. From
    /// no line, the first key lands on the top of what is shown.
    pub fn move_line(&mut self, mv: LineMove, cx: &mut Context<Self>) {
        let count = self.lines.len();
        if count == 0 {
            return;
        }
        let last = count.saturating_sub(1);
        let at = i64::try_from(self.focus.unwrap_or_else(|| self.top_line())).unwrap_or(0);
        let to = match (mv, self.focus) {
            (LineMove::Lines(_) | LineMove::Pages(_), None) => at,
            (LineMove::Lines(n), Some(_)) => at.saturating_add(n),
            (LineMove::Pages(n), Some(_)) => at.saturating_add(n.saturating_mul(self.page_lines())),
            (LineMove::First, _) => 0,
            (LineMove::Last, _) => i64::try_from(last).unwrap_or(i64::MAX),
        };
        let to = usize::try_from(to.max(0)).unwrap_or(0).min(last);
        self.focus = Some(to);
        self.scroll.scroll_to_item(to, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// The topmost row shown, from the list's last layout (a pending scroll counts).
    fn top_line(&self) -> usize {
        let state = self.scroll.0.borrow();
        state
            .deferred_scroll_to_item
            .as_ref()
            .map_or_else(|| state.base_handle.logical_scroll_top().0, |d| d.item_index)
    }

    /// Rows the list shows at once, from its last layout; one before any.
    fn page_lines(&self) -> i64 {
        let state = self.scroll.0.borrow();
        let rows = state.last_item_size.map_or(1.0, |size| {
            let row = f32::from(size.item.height).max(1.0);
            (f32::from(state.base_handle.bounds().size.height) / row).floor()
        });
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a floored row count clamped to [1, 10 000] fits an i64 exactly"
        )]
        let rows = rows.clamp(1.0, 10_000.0) as i64;
        rows
    }

    /// ⌘F: open the find bar, or put the caret back in it with the text selected.
    pub fn find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("find"));
            let subscription = cx.subscribe(&input, |this, _input, event, cx| match event {
                InputEvent::Change => this.search_changed(cx),
                InputEvent::PressEnter { shift, .. } => {
                    this.step_hit(if *shift { -1 } else { 1 }, cx);
                }
                InputEvent::Focus | InputEvent::Blur => {}
            });
            self.search = Some(FileSearch {
                input,
                needle: String::new(),
                hits: Vec::new(),
                current: None,
                _subscription: subscription,
            });
        }
        if let Some(search) = &self.search {
            search.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
        cx.notify();
    }

    /// Open the find bar on `needle` (a find in every card chose this one): the field holds
    /// it and the card lands on the first hit.
    pub fn find_with(&mut self, needle: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.find(window, cx);
        let Some(search) = &mut self.search else { return };
        search.input.update(cx, |input, cx| input.set_value(needle.to_owned(), window, cx));
        needle.clone_into(&mut search.needle);
        self.refresh_hits(cx);
    }

    /// The find bar's needle, when the bar is open.
    #[must_use]
    pub fn search_needle(&self) -> Option<&str> {
        self.search.as_ref().map(|s| s.needle.as_str())
    }

    /// The text's lines as drawn (empty until the host answers).
    #[must_use]
    pub fn lines(&self) -> &[SharedString] {
        &self.lines
    }

    /// Esc or ✕ in the find bar: close it; the canvas takes the keyboard back.
    pub fn close_find(&mut self, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            cx.emit(FileViewEvent::FindClosed);
            cx.notify();
        }
    }

    /// Whether the find bar is open.
    #[must_use]
    pub const fn finding(&self) -> bool {
        self.search.is_some()
    }

    /// The line the human is on, 1-based, for an editor: the current hit while finding, else
    /// the line the card opened at, else none.
    #[must_use]
    pub fn reading_line(&self) -> Option<u32> {
        let hit = self.search.as_ref().and_then(|s| s.current.and_then(|c| s.hits.get(c).copied()));
        hit.or(self.focus).and_then(|ix| u32::try_from(ix.saturating_add(1)).ok())
    }

    /// The lines found (indices into what is drawn) and which one the card is on.
    #[must_use]
    pub fn hits(&self) -> Option<(&[usize], Option<usize>)> {
        self.search.as_ref().map(|s| (s.hits.as_slice(), s.current))
    }

    /// The field changed: the hits follow, the card landing on the first one at or after
    /// the line it opened at (an edit's neighbourhood is where a search usually starts).
    fn search_changed(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        let needle = search.input.read(cx).value().to_string();
        if needle == search.needle {
            return;
        }
        search.needle = needle;
        self.refresh_hits(cx);
    }

    /// Recount the hits for the needle (it, or the text, changed) and land on the first.
    fn refresh_hits(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        search.hits = find_hits(&self.lines, &search.needle);
        let from = self.focus.unwrap_or(0);
        search.current = if search.hits.is_empty() {
            None
        } else {
            Some(search.hits.iter().position(|&line| line >= from).unwrap_or(0))
        };
        self.scroll_to_hit();
        cx.notify();
    }

    /// ⌘G / ↩ (+1) and ⌘⇧G / ⇧↩ (−1), wrapping.
    pub fn step_hit(&mut self, delta: i64, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        let count = i64::try_from(search.hits.len()).unwrap_or(0);
        if count == 0 {
            return;
        }
        let at = i64::try_from(search.current.unwrap_or(0)).unwrap_or(0);
        search.current = usize::try_from(at.saturating_add(delta).rem_euclid(count)).ok();
        self.scroll_to_hit();
        cx.notify();
    }

    fn scroll_to_hit(&self) {
        let Some(search) = &self.search else { return };
        if let Some(line) = search.current.and_then(|c| search.hits.get(c)) {
            self.scroll.scroll_to_item(*line, ScrollStrategy::Center);
        }
    }

    /// The find bar over the top-right corner, as the terminal's.
    fn render_search(&self, search: &FileSearch, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (spacing, radii) = (theme.spacing, theme.radii);
        let wash = hsla_alpha(s.text, alpha::HOVER);
        let bare = move |id: &'static str| {
            div()
                .id(id)
                .px(px(spacing.xs))
                .rounded(px(radii.xs))
                .cursor_pointer()
                .text_color(hsla(s.text_muted))
                .hover(move |st| st.bg(wash))
        };
        let count: SharedString = if search.needle.is_empty() {
            SharedString::default()
        } else if search.hits.is_empty() {
            "none".into()
        } else {
            let at = search.current.map_or(0, |c| c.saturating_add(1));
            format!("{at}/{}", search.hits.len()).into()
        };
        div()
            .id("file-search")
            .debug_selector(|| "file-search".to_owned())
            .key_context("FileSearch")
            .absolute()
            .top(px(spacing.sm))
            .right(px(spacing.sm))
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .px(px(spacing.sm))
            .py(px(spacing.xs))
            .rounded(px(radii.sm))
            .bg(hsla(s.panel))
            .border_1()
            .border_color(hsla(s.border))
            .shadow_sm()
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text))
            .font_family(theme.typography.ui_family.clone())
            .on_action(cx.listener(|this, _: &CloseFind, _window, cx| this.close_find(cx)))
            .on_action(cx.listener(|this, _: &FindNext, _window, cx| this.step_hit(1, cx)))
            .on_action(cx.listener(|this, _: &FindPrev, _window, cx| this.step_hit(-1, cx)))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .role(Role::Group)
            .aria_label("Find in file")
            .child(div().w(px(180.0)).child(Input::new(&search.input).aria_label("Find in file")))
            .child(
                div()
                    .id("file-search-count")
                    .min_w(px(40.0))
                    .text_color(hsla(s.text_secondary))
                    .role(Role::Label)
                    .aria_label("Matches")
                    .aria_value(count.clone())
                    .child(count),
            )
            .child(
                bare("file-search-prev")
                    .role(Role::Button)
                    .aria_label("Previous match")
                    .child("↑")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.step_hit(-1, cx))),
            )
            .child(
                bare("file-search-next")
                    .role(Role::Button)
                    .aria_label("Next match")
                    .child("↓")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.step_hit(1, cx))),
            )
            .child(
                bare("file-search-close")
                    .role(Role::Button)
                    .aria_label("Close find")
                    .child("✕")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.close_find(cx))),
            )
            .into_any_element()
    }

    /// Item this card belongs to.
    #[must_use]
    pub const fn id(&self) -> ItemId {
        self.id
    }

    /// The path on the host.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// What the host said, once it has.
    #[must_use]
    pub const fn read(&self) -> Option<&FileRead> {
        self.read.as_ref()
    }

    /// Lines drawn.
    #[must_use]
    pub const fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The lines the last read changed, indices into what is drawn.
    #[must_use]
    pub fn changed(&self) -> &[usize] {
        &self.changed
    }

    /// The line the card was opened at, an index into what is drawn.
    #[must_use]
    pub const fn focus(&self) -> Option<usize> {
        self.focus
    }

    /// Land on `line` (1-based, as a tool names it): tinted, and scrolled into view now if
    /// the text is here, else when it arrives. `None` clears it.
    pub fn focus_line(&mut self, line: Option<u32>, cx: &mut Context<Self>) {
        self.focus = line.and_then(|l| usize::try_from(l).ok()).map(|l| l.saturating_sub(1));
        if let Some(at) = self.focus
            && !self.lines.is_empty()
        {
            self.scroll
                .scroll_to_item(at.min(self.lines.len().saturating_sub(1)), ScrollStrategy::Center);
        }
        cx.notify();
    }

    /// The host answered (or answered again after an edit). A second text after a first one
    /// marks the lines that differ and scrolls to the first of them.
    pub fn set_read(&mut self, read: FileRead, cx: &mut Context<Self>) {
        if self.read.as_ref() == Some(&read) {
            return;
        }
        let (lines, text): (Vec<SharedString>, Arc<str>) = match &read {
            FileRead::Text { text, .. } => (
                text.split('\n').map(|l| SharedString::from(l.to_owned())).collect(),
                Arc::from(text.as_str()),
            ),
            FileRead::Binary { .. } | FileRead::Missing { .. } => (Vec::new(), Arc::from("")),
        };
        let had_text = matches!(self.read, Some(FileRead::Text { .. }));
        self.changed = if had_text && !lines.is_empty() {
            changed_lines(&self.lines, &lines)
        } else {
            Vec::new()
        };
        // A changed line is the reason for this read; the opening line is where a first text
        // lands.
        let land = self.changed.first().copied().or(if had_text { None } else { self.focus });
        if let Some(at) = land
            && !lines.is_empty()
        {
            self.scroll
                .scroll_to_item(at.min(lines.len().saturating_sub(1)), ScrollStrategy::Center);
        }
        self.lines = lines;
        self.read = Some(read);
        self.recolour(text, cx);
        // An open find bar follows the new text.
        if self.search.is_some() {
            self.refresh_hits(cx);
        }
        cx.notify();
    }

    /// Paint scale (the canvas's zoom) and the theme's inset and type size at scale 1.
    pub const fn set_layout(&mut self, zoom: f32, pad: f32, text_size: f32) {
        self.zoom = zoom;
        self.pad = pad;
        self.text_size = text_size;
    }

    /// Draw by another theme (the canvas swapped it).
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme == theme {
            return;
        }
        self.theme = theme;
        cx.notify();
    }

    /// One line about the file for a screen reader and the collapsed card: "212 lines",
    /// "212 lines, 40 more", "binary, 1.2 MB", "missing: No such file".
    #[must_use]
    pub fn summary(&self) -> String {
        match &self.read {
            None => "reading…".to_owned(),
            Some(FileRead::Text { more_lines, .. }) => {
                let n = self.lines.len();
                let lines = if n == 1 { "1 line".to_owned() } else { format!("{n} lines") };
                let more =
                    if *more_lines > 0 { format!(", {more_lines} more") } else { String::new() };
                let coloured =
                    self.coloured_as().map_or_else(String::new, |name| format!(", {name}"));
                let changed = match self.changed.len() {
                    0 => String::new(),
                    c => format!(", {c} changed"),
                };
                format!("{lines}{more}{changed}{coloured}")
            }
            Some(FileRead::Binary { size }) => format!("binary, {}", size_label(*size)),
            Some(FileRead::Missing { error }) => format!("missing: {error}"),
        }
    }

    fn notice(&self, text: String) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(self.pad * self.zoom))
            .text_size(px(self.text_size * self.zoom))
            .text_color(hsla(self.theme.surfaces.text_muted))
            .child(SharedString::from(text))
            .into_any_element()
    }
}

/// Indices into `new` of the lines that differ from `old`.
///
/// Every inserted or replaced line, and for a pure deletion the line now standing where the
/// deleted ones were (clamped to the last line), so a deletion is still pointed at.
#[must_use]
pub fn changed_lines(old: &[SharedString], new: &[SharedString]) -> Vec<usize> {
    let old: Vec<&str> = old.iter().map(SharedString::as_ref).collect();
    let new: Vec<&str> = new.iter().map(SharedString::as_ref).collect();
    let diff = similar::TextDiff::from_slices(&old, &new);
    let mut changed = Vec::new();
    for op in diff.ops() {
        match *op {
            similar::DiffOp::Equal { .. } => {}
            similar::DiffOp::Insert { new_index, new_len, .. }
            | similar::DiffOp::Replace { new_index, new_len, .. } => {
                changed.extend(new_index..new_index.saturating_add(new_len));
            }
            similar::DiffOp::Delete { new_index, .. } => {
                if !new.is_empty() {
                    changed.push(new_index.min(new.len().saturating_sub(1)));
                }
            }
        }
    }
    changed.dedup();
    changed
}

/// A byte count as a human reads it.
#[must_use]
pub fn size_label(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    #[expect(clippy::cast_precision_loss, reason = "a label, not arithmetic")]
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

impl Render for FileView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = *self.id.as_uuid();
        let search = self.search.as_ref().map(|s| self.render_search(s, cx));
        let theme = self.theme.clone();
        let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
        let text_size = self.text_size * self.zoom;
        let pad = self.pad * self.zoom;
        let body = match &self.read {
            None => self.notice("reading…".to_owned()),
            Some(FileRead::Binary { .. } | FileRead::Missing { .. }) => self.notice(self.summary()),
            Some(FileRead::Text { more_lines, .. }) => {
                let more = *more_lines;
                let count = self.lines.len();
                let digits = count.max(1).to_string().len();
                // The gutter is as wide as the last line number, in the mono face.
                let gutter_ch = f32::from(u8::try_from(digits).unwrap_or(u8::MAX));
                let lines = self.lines.clone();
                let spans = self.spans.clone();
                let font = crate::fonts::terminal_font(&mono, false, false);
                let run_theme = theme.clone();
                let changed = self.changed.clone();
                let focus = self.focus;
                let (hits, current) = self.search.as_ref().map_or((Vec::new(), None), |s| {
                    (s.hits.clone(), s.current.and_then(|c| s.hits.get(c).copied()))
                });
                let muted = hsla(theme.surfaces.text_muted);
                let fg = hsla(theme.surfaces.text);
                let tint = hsla_alpha(theme.surfaces.success, alpha::TINT);
                let mark = hsla_alpha(theme.surfaces.accent, alpha::TINT);
                let hit = hsla_alpha(theme.surfaces.warn, alpha::TINT);
                let here = hsla_alpha(theme.surfaces.warn, alpha::TINT_STRONG);
                let this = cx.entity().downgrade();
                let list = uniform_list(
                    SharedString::from(format!("file-lines-{id}")),
                    count,
                    move |range, _window, _cx| {
                        range
                            .filter_map(|ix| {
                                let line = lines.get(ix)?.clone();
                                // A line with no spans (a plain file, or the parse still
                                // running) is plain text, sparing the run vector; the
                                // coloured card's cost was the shaper's, not this (MEASUREMENTS
                                // 2026-09-13, "a coloured file card's zoom").
                                let text = match spans.as_ref().and_then(|s| s.get(ix)) {
                                    Some(line_spans) => StyledText::new(line.clone())
                                        .with_runs(highlight::runs(
                                            line.len(),
                                            Some(line_spans.as_slice()),
                                            &font,
                                            &run_theme,
                                        ))
                                        .into_any_element(),
                                    None => line.into_any_element(),
                                };
                                let number = ix.saturating_add(1);
                                let this = this.clone();
                                Some(
                                    div()
                                        .id(ElementId::NamedInteger(
                                            "file-line".into(),
                                            u64::try_from(ix).unwrap_or(u64::MAX),
                                        ))
                                        .debug_selector(move || format!("file-line-{id}-{ix}"))
                                        // A click (a tap) makes the line the reading line.
                                        .on_click(move |_ev, _window, cx| {
                                            let _set = this.update(cx, |v, cx| {
                                                v.focus_line(u32::try_from(number).ok(), cx);
                                            });
                                        })
                                        .flex()
                                        .gap(px(pad))
                                        .whitespace_nowrap()
                                        .when(focus == Some(ix), |el| el.bg(mark))
                                        .when(changed.binary_search(&ix).is_ok(), |el| el.bg(tint))
                                        .when(hits.binary_search(&ix).is_ok(), |el| el.bg(hit))
                                        .when(current == Some(ix), |el| el.bg(here))
                                        .child(
                                            div()
                                                .flex_none()
                                                .w(px(gutter_ch * text_size * 0.62))
                                                .text_color(muted)
                                                .child(SharedString::from(format!(
                                                    "{number:>digits$}"
                                                ))),
                                        )
                                        .child(div().text_color(fg).child(text)),
                                )
                            })
                            .collect()
                    },
                )
                .track_scroll(&self.scroll)
                .flex_1()
                .w_full();
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .px(px(pad))
                    .py(px(pad / 2.0))
                    .child(list)
                    .when(more > 0, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .italic()
                                .text_color(hsla(theme.surfaces.text_muted))
                                .child(SharedString::from(format!("{more} more lines"))),
                        )
                    })
                    .into_any_element()
            }
        };
        div()
            .id(SharedString::from(format!("file-{id}")))
            .debug_selector(move || format!("file-{id}"))
            .role(Role::Document)
            .aria_label(SharedString::from(format!("File {}", self.path)))
            .aria_value(SharedString::from(self.summary()))
            .size_full()
            .overflow_hidden()
            .font_family(mono)
            .text_size(px(text_size))
            .child(body)
            .children(search)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_are_the_lines_holding_the_needle_in_any_case() {
        let lines: Vec<SharedString> = ["Alpha", "beta", "alpha beta", "gamma"]
            .iter()
            .map(|l| SharedString::from(*l))
            .collect();
        assert_eq!(find_hits(&lines, "alpha"), [0, 2]);
        assert_eq!(find_hits(&lines, "BETA"), [1, 2]);
        assert_eq!(find_hits(&lines, "delta"), Vec::<usize>::new());
        assert_eq!(find_hits(&lines, ""), Vec::<usize>::new(), "an empty needle finds nothing");
    }

    #[test]
    fn changed_lines_point_at_inserts_replacements_and_deletions() {
        let l =
            |v: &[&str]| v.iter().map(|s| SharedString::from((*s).to_owned())).collect::<Vec<_>>();
        assert_eq!(changed_lines(&l(&["a", "b", "c"]), &l(&["a", "B", "c"])), [1]);
        assert_eq!(changed_lines(&l(&["a", "c"]), &l(&["a", "b", "b2", "c"])), [1, 2]);
        assert_eq!(changed_lines(&l(&["a", "b", "c"]), &l(&["a", "c"])), [1]);
        assert_eq!(
            changed_lines(&l(&["a", "b"]), &l(&["a"])),
            [0],
            "a deleted tail points at the last line"
        );
        assert_eq!(changed_lines(&l(&["a"]), &l(&["a"])), Vec::<usize>::new());
    }

    /// A Rust file is coloured once the background parse lands; a plain file is not; a read
    /// that follows drops the earlier colours and takes the new ones.
    #[gpui::test]
    fn a_file_is_coloured_by_its_grammar_after_the_read(cx: &mut gpui::TestAppContext) {
        let read = |text: &str| FileRead::Text {
            text: text.to_owned(),
            more_lines: 0,
            size: u64::try_from(text.len()).unwrap_or(0),
            modified_ms: 0,
        };
        let coloured = |v: &FileView, _: &gpui::App| v.coloured_as();
        let view = cx.new(|_| FileView::new(ItemId::new(), "/w/src/main.rs", Theme::default()));
        view.update(cx, |v, cx| v.set_read(read("fn main() {}\n// end"), cx));
        assert_eq!(view.read_with(cx, coloured), None, "not before the parse");
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, coloured), Some("Rust"));
        view.read_with(cx, |v, _| {
            let spans = v.spans.as_deref().unwrap_or_default();
            assert_eq!(spans.len(), 2);
            let first = |ix: usize| spans.get(ix).and_then(|l| l.first());
            assert_eq!(first(0).map(|s| s.token), Some(highlight::Token::Keyword));
            assert_eq!(first(1).map(|s| s.italic), Some(true), "a comment is italic");
            assert_eq!(v.summary(), "2 lines, Rust");
        });
        view.update(cx, |v, cx| v.set_read(read("fn main() {}\n// end\nlet"), cx));
        assert_eq!(view.read_with(cx, coloured), None, "a new text starts over");
        cx.run_until_parked();
        view.read_with(cx, |v, _| assert_eq!(v.spans.as_ref().map(|s| s.len()), Some(3)));

        let plain =
            cx.new(|_| FileView::new(ItemId::new(), "/w/notes.unknownext", Theme::default()));
        plain.update(cx, |v, cx| v.set_read(read("just words"), cx));
        cx.run_until_parked();
        assert_eq!(plain.read_with(cx, coloured), None);
        assert_eq!(plain.read_with(cx, |v, _| v.summary()), "1 line");
    }

    #[test]
    fn sizes_read_as_a_human_would() {
        assert_eq!(size_label(512), "512 B");
        assert_eq!(size_label(1536), "1.5 KB");
        assert_eq!(size_label(3 * 1024 * 1024), "3.0 MB");
    }
}
