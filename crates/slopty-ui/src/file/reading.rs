//! A Markdown file read as it renders: prose, task boxes that tick and fenced blocks that copy
//! and run, the source one toggle away.
//!
//! A `.md` file tile opens on the preview once its text is in, unless it was opened to edit: at a
//! line, under a program waiting on it, or with nothing written yet. ⌘⇧V, the header's toggle
//! and the palette swap it with the source, and anything that works on the source (find, go to
//! line) swaps to it first. The preview draws the editor's text, so an edit not
//! saved yet shows in it too. A box ticked in the preview changes its line in the editor, and a
//! file that was saved before the tick is saved again at once, since a tick is the whole edit.
//!
//! The rows are the text's segments ([`crate::markdown::segments`]) in a gpui `list`, so a long
//! file lays out only the rows in view, and a change measures again only the rows it touched.

use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ListAlignment, ListState,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    list, px,
};
use gpui_kit::component::text::TextView;

use super::{FileView, FileViewEvent};
use crate::markdown::Segment;

/// How far past the view's edges the preview measures its rows, in points, so a scroll lands on
/// rows already sized.
const OVERDRAW: f32 = 512.0;

/// The longest line the preview sets, in points: the thread's reading column.
const MEASURE: f32 = crate::conversation::thread::view::COLUMN;

/// What the header's toggle says while the preview shows.
pub const SHOW_SOURCE: &str = "Show source";
/// What the header's toggle says while the source shows.
pub const SHOW_PREVIEW: &str = "Show preview";

/// The extensions a Markdown file goes by, lower case.
const EXTENSIONS: [&str; 4] = ["md", "markdown", "mdown", "mkd"];

/// Whether `path` names a Markdown file.
#[must_use]
pub fn is_markdown(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e))
    })
}

/// A Markdown tile's preview: whether it shows, and its rows.
pub(super) struct Reading {
    /// The preview shows rather than the source.
    on: bool,
    /// Settled at the first text: a later read keeps the person's choice.
    settled: bool,
    segments: Rc<[Segment]>,
    /// The text `segments` were read from.
    segmented: String,
    list: ListState,
}

impl Reading {
    pub(super) fn new() -> Self {
        Self {
            on: false,
            settled: false,
            segments: Rc::from([]),
            segmented: String::new(),
            list: ListState::new(0, ListAlignment::Top, px(OVERDRAW)),
        }
    }

    /// Read `text` into the rows, if it changed. Only the rows between the first and the last
    /// that differ are measured again, so ticking a box keeps the place the preview is at.
    fn segment(&mut self, text: &str) {
        if text == self.segmented {
            return;
        }
        let new = crate::markdown::segments(text);
        let old = &self.segments;
        let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let (old_rest, new_rest) = (old.get(head..).unwrap_or(&[]), new.get(head..).unwrap_or(&[]));
        let tail =
            old_rest.iter().rev().zip(new_rest.iter().rev()).take_while(|(a, b)| a == b).count();
        let changed = old_rest.len().saturating_sub(tail);
        self.list.splice(head..head.saturating_add(changed), new_rest.len().saturating_sub(tail));
        self.segments = Rc::from(new);
        text.clone_into(&mut self.segmented);
    }

    /// Show the source from now on, whatever the first text: the tile was opened to edit.
    pub(super) const fn keep_source(&mut self) {
        self.on = false;
        self.settled = true;
    }

    /// Lay the rows out again (the theme or the run button changed).
    pub(super) fn remeasure(&self) {
        self.list.remeasure();
    }
}

impl FileView {
    /// Whether the tile is a Markdown file's, which has a preview.
    #[must_use]
    pub const fn has_preview(&self) -> bool {
        self.reading.is_some()
    }

    /// Whether the preview shows now, in place of the source.
    #[must_use]
    pub fn previewing(&self) -> bool {
        self.reading.as_ref().is_some_and(|r| r.on) && self.shows_text() && self.comparing.is_none()
    }

    /// Show the preview (`on`) or the source, the keyboard moving with it.
    pub fn show_preview(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(reading) = self.reading.as_mut() else { return };
        reading.settled = true;
        if reading.on == on {
            return;
        }
        reading.on = on;
        tracing::debug!(path = %self.path, on, "markdown preview");
        if on {
            self.search = None;
            self.goto = None;
            window.focus(&self.focus_handle, cx);
        } else {
            self.editor.update(cx, |e, cx| e.focus(window, cx));
        }
        cx.notify();
    }

    /// ⌘⇧V: the preview for the source, or back.
    pub fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let on = !self.previewing();
        self.show_preview(on, window, cx);
    }

    /// The first text is in: a Markdown tile shows its preview, unless it was opened to edit.
    pub(super) fn settle_preview(&mut self, cx: &gpui::App) {
        let editing = self.focus.is_some() || self.waiting.is_some() || self.is_new();
        let empty = self.text(cx).trim().is_empty();
        let Some(reading) = self.reading.as_mut().filter(|r| !r.settled) else { return };
        reading.settled = true;
        reading.on = !editing && !empty;
    }

    /// Whether a fenced block's "run" can go to a shell.
    pub fn set_can_run(&mut self, can: bool, cx: &mut Context<Self>) {
        if self.can_run != can {
            self.can_run = can;
            if let Some(reading) = &self.reading {
                reading.remeasure();
            }
            cx.notify();
        }
    }

    /// Tick or untick the `ix`-th task line from its box: the line changes in the editor, and a
    /// file with nothing else unsaved is saved at once.
    pub fn toggle_task(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let Some(flipped) = crate::markdown::toggle_task(&text, ix) else { return };
        let clean = !self.dirty && self.trouble.is_none();
        self.editor.update(cx, |e, cx| e.set_value(flipped, window, cx));
        self.edited(cx);
        if clean {
            self.save(cx);
        }
    }

    /// The preview: the editor's text drawn as Markdown.
    pub(super) fn render_reading(&mut self, cx: &Context<Self>) -> AnyElement {
        let text = self.text(cx);
        let id = *self.id.as_uuid();
        if let Some(reading) = self.reading.as_mut() {
            reading.segment(&text);
        }
        let Some(reading) = self.reading.as_ref() else { return div().into_any_element() };
        let rows = list(
            reading.list.clone(),
            cx.processor(|this, ix: usize, _window, cx| this.render_segment(ix, cx)),
        )
        .size_full();
        let pad = px(self.pad);
        div()
            .id(SharedString::from(format!("file-preview-{id}")))
            .debug_selector(move || format!("file-preview-{id}"))
            // gpui-kit draws Markdown as styled text, which leaves nothing in the accessibility
            // tree: the preview reads the text out, as the editor does.
            .role(Role::Document)
            .aria_label(SharedString::from(format!("Preview of {}", self.path)))
            .aria_value(SharedString::from(text))
            .flex_1()
            .min_h_0()
            .w_full()
            .p(pad)
            .overflow_hidden()
            .font_family(self.theme.typography.ui_family.clone())
            .text_size(px(self.theme.typography.ui_size))
            .line_height(gpui::relative(self.theme.typography.markdown_line_height))
            .child(rows)
            .into_any_element()
    }

    /// The `ix`-th row of the preview, spaced from the one above.
    fn render_segment(&self, ix: usize, cx: &Context<Self>) -> AnyElement {
        let Some(segment) = self.reading.as_ref().and_then(|r| r.segments.get(ix)) else {
            return div().into_any_element();
        };
        let id = self.id.as_uuid();
        let mono = self.theme.typography.mono_families.first().cloned().unwrap_or_default();
        let row = match segment {
            Segment::Prose(prose) => TextView::markdown(
                SharedString::from(format!("file-md-{id}-{ix}")),
                SharedString::from(prose.clone()),
            )
            .style(crate::markdown::style(&self.theme, &mono))
            .selectable(true)
            .into_any_element(),
            Segment::Task(task) => {
                let file = cx.entity();
                let toggle: crate::markdown::Toggle =
                    Rc::new(move |ix, window, cx: &mut gpui::App| {
                        file.update(cx, |f, cx| f.toggle_task(ix, window, cx));
                    });
                crate::markdown::task_row(
                    format!("file-task-{id}-{}", task.ix),
                    task,
                    &self.theme,
                    &mono,
                    Some(toggle),
                )
            }
            Segment::Code { lang, body } => {
                let run = self.can_run.then(|| -> crate::markdown::Run {
                    let file = cx.entity();
                    Rc::new(move |code: String, cx: &mut gpui::App| {
                        file.update(cx, |_f, cx| cx.emit(FileViewEvent::RunBlock(code)));
                    })
                });
                crate::markdown::code_block(
                    (format!("file-code-copy-{id}-{ix}"), format!("file-code-run-{id}-{ix}")),
                    lang,
                    body,
                    &self.theme,
                    run,
                )
            }
        };
        let gap = if ix == 0 { 0.0 } else { self.theme.spacing.xs };
        // Centred on the thread's reading measure, so a wide tile keeps lines a reader's eye
        // can follow back, and a long file reads as an agent's answer does.
        div()
            .w_full()
            .flex()
            .justify_center()
            .child(
                div()
                    .debug_selector(move || format!("file-measure-{id}-{ix}"))
                    .w_full()
                    .max_w(px(MEASURE))
                    .pt(px(gap))
                    .child(row),
            )
            .into_any_element()
    }
}
