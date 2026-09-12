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
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use slopty_core::ItemId;
use slopty_proto::file::FileRead;
use slopty_theme::{Theme, alpha};

use crate::colors::{hsla, hsla_alpha};

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
        }
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
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id.as_uuid();
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
                let muted = hsla(theme.surfaces.text_muted);
                let fg = hsla(theme.surfaces.text);
                let tint = hsla_alpha(theme.surfaces.success, alpha::TINT);
                let mark = hsla_alpha(theme.surfaces.accent, alpha::TINT);
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
