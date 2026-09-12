//! A file card on the canvas: a file on the host, read-only, as the agent left it.
//!
//! The item (`ItemKind::File`) names the path; the text is not in the document. Each client
//! asks the host for it (`ClientMsg::ReadFile`) when the card appears and again when an agent's
//! edit or write lands, and draws what came back: line-numbered mono rows in a `uniform_list`
//! (a 2 000-line file lays out only the rows on screen), or one line saying why there is
//! nothing to draw. A read that follows one (an agent's edit landed, "reload" pressed) tints
//! the lines that changed and scrolls the first of them into view, so the card shows what the
//! agent just did without the human hunting for it.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, UniformListScrollHandle, Window,
    div, px, uniform_list,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use slopty_core::ItemId;
use slopty_proto::file::FileRead;
use slopty_theme::{Theme, alpha};

use crate::colors::{hsla, hsla_alpha};
use crate::terminal::{CloseFind, FindNext, FindPrev};

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
        }
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
        let lines: Vec<SharedString> = match &read {
            FileRead::Text { text, .. } => {
                text.split('\n').map(|l| SharedString::from(l.to_owned())).collect()
            }
            FileRead::Binary { .. } | FileRead::Missing { .. } => Vec::new(),
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
                let changed = match self.changed.len() {
                    0 => String::new(),
                    c => format!(", {c} changed"),
                };
                format!("{lines}{more}{changed}")
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
        let id = self.id.as_uuid();
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
                let list = uniform_list(
                    SharedString::from(format!("file-lines-{id}")),
                    count,
                    move |range, _window, _cx| {
                        range
                            .filter_map(|ix| {
                                let line = lines.get(ix)?.clone();
                                let number = ix.saturating_add(1);
                                Some(
                                    div()
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
                                        .child(div().text_color(fg).child(line)),
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

    #[test]
    fn sizes_read_as_a_human_would() {
        assert_eq!(size_label(512), "512 B");
        assert_eq!(size_label(1536), "1.5 KB");
        assert_eq!(size_label(3 * 1024 * 1024), "3.0 MB");
    }
}
