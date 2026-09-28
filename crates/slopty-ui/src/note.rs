//! A sticky note in the workspace: Markdown read, free text edited in place, stored in the
//! shared document.
//!
//! The text lives in the worker's item (`ItemKind::Note`), so every client sees it. A note that
//! is not being edited draws its text as Markdown ([`crate::markdown::style`]); a click or the
//! workspace putting the caret in it swaps in the editor, and blur swaps back. Edits are committed
//! to the worker after a short pause in typing and on blur; a remote change is taken only while
//! this client is not editing (last writer wins, no merge). A task line (`- [ ] …`) is read as a
//! row whose box ticks on a click, the one edit that needs no editor.
//!
//! The Markdown is a list of segments (prose, a task, a fenced block) in a gpui `list`, so a
//! frame lays out and paints only the rows in view, and a long note scrolls. Each segment
//! carries a `TextView`, which tracks a focus handle: drawn whole, a 64 KiB note of task lines
//! put about 3 200 of them in the window's tab-stop map, and every frame that replayed the note
//! inserted them all again.

use std::rc::Rc;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ListAlignment, ListState, MouseButton,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, list, px,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Sizable as _, Size};
use slopty_core::ItemId;
use slopty_theme::Theme;

use crate::markdown::Segment;

/// Typing pause before the text is sent to the worker.
const COMMIT_AFTER: Duration = Duration::from_millis(400);

/// How far past the view's edges the reader measures its rows, in points, so a scroll lands on
/// rows already sized. Rows there are laid out once, not painted.
const OVERDRAW: f32 = 512.0;

/// What an empty note says.
pub(crate) const WRITE_PLACEHOLDER: &str = "Write…";

/// What a note tells the workspace.
#[derive(Clone, Debug)]
pub enum NoteViewEvent {
    /// The text changed and should be written to the document.
    Commit(String),
    /// A fenced block's "run" button: type its code into the workspace's shell.
    Run(String),
}

/// The editor for one note item.
pub struct NoteView {
    id: ItemId,
    text: Entity<TextareaState>,
    /// Text as last committed to or received from the document.
    synced: String,
    /// The document's text, changed by another client while this one was editing: taken on
    /// blur if nothing was typed meanwhile.
    offered: Option<String>,
    /// Bumped on every change; a scheduled commit only fires if it is still current.
    generation: u64,
    zoom: f32,
    /// Inner padding at zoom 1 (the theme's base spacing).
    pad: f32,
    /// Text size at zoom 1 (the theme's UI size).
    text_size: f32,
    /// The workspace has a shell to run a fenced block in (the "run" button shows only then).
    can_run: bool,
    theme: Theme,
    /// The reader's rows: the segments after the title, as last read from the text.
    segments: Rc<[Segment]>,
    /// The text `segments` were read from.
    segmented: String,
    list: ListState,
    /// Times this view was rendered rather than replayed from the view cache (tests).
    #[cfg(test)]
    renders: u32,
    /// The text the last render read (tests).
    #[cfg(test)]
    drawn: String,
    _subscriptions: [gpui::Subscription; 2],
}

impl std::fmt::Debug for NoteView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoteView")
            .field("id", &self.id)
            .field("synced", &self.synced)
            .field("generation", &self.generation)
            .field("zoom", &self.zoom)
            .finish_non_exhaustive()
    }
}

impl NoteView {
    /// A note showing `text`.
    pub fn new(
        id: ItemId,
        text: &str,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(WRITE_PLACEHOLDER)
                .default_value(text.to_owned())
        });
        let subscription =
            cx.subscribe_in(&state, window, |this, _state, event, window, cx| match event {
                InputEvent::Change => this.schedule_commit(cx),
                // Focus and blur swap the reader for the editor and back.
                InputEvent::Blur => this.blurred(window, cx),
                InputEvent::Focus => cx.notify(),
                InputEvent::PressEnter { .. } => {}
            });
        // The note is drawn from a cached view: whatever changes its editor (a key, a caret
        // blink, a selection) draws the note afresh.
        let typing = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            id,
            text: state,
            synced: text.to_owned(),
            offered: None,
            generation: 0,
            zoom: 1.0,
            pad: 8.0,
            text_size: 13.0,
            can_run: false,
            theme,
            segments: Rc::from([]),
            segmented: String::new(),
            list: ListState::new(0, ListAlignment::Top, px(OVERDRAW)),
            #[cfg(test)]
            renders: 0,
            #[cfg(test)]
            drawn: String::new(),
            _subscriptions: [subscription, typing],
        }
    }

    /// Item this note belongs to.
    #[must_use]
    pub const fn id(&self) -> ItemId {
        self.id
    }

    /// Whether the workspace has a shell to run a fenced block in: with one, a block that is
    /// read (not edited) carries a "run" button beside its "copy".
    pub fn set_can_run(&mut self, can: bool, cx: &mut Context<Self>) {
        if self.can_run != can {
            self.can_run = can;
            self.list.remeasure();
            cx.notify();
        }
    }

    /// The text as last committed or received.
    #[must_use]
    pub fn synced(&self) -> &str {
        &self.synced
    }

    /// What the field holds this instant, keystrokes the commit timer has not carried into
    /// the document included. The workspace reads this when it has to act on the note's text
    /// before that timer fires — closing the tile, which must take back what was typed.
    #[must_use]
    pub fn live_text(&self, cx: &gpui::App) -> String {
        self.text.read(cx).value().to_string()
    }

    /// Whether the editor has keyboard focus.
    #[must_use]
    pub fn editing(&self, window: &Window, cx: &gpui::App) -> bool {
        self.text.read(cx).focus_handle(cx).is_focused(window)
    }

    /// The overview's zoom, which the note is drawn at, and the theme's inset and type size at
    /// scale 1. The note is drawn from a cached view, so a change here draws it afresh.
    pub fn set_layout(&mut self, zoom: f32, pad: f32, text_size: f32, cx: &mut Context<Self>) {
        let changed = [(self.zoom, zoom), (self.pad, pad), (self.text_size, text_size)]
            .iter()
            .any(|(was, now)| (was - now).abs() > f32::EPSILON);
        if changed {
            self.zoom = zoom;
            self.pad = pad;
            self.text_size = text_size;
            self.list.remeasure();
            cx.notify();
        }
    }

    /// Draw by another theme (the workspace swapped it).
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme == theme {
            return;
        }
        self.theme = theme;
        self.list.remeasure();
        cx.notify();
    }

    /// Times this view was rendered rather than replayed from the view cache.
    #[cfg(test)]
    pub const fn renders(&self) -> u32 {
        self.renders
    }

    /// The text the last render read.
    #[cfg(test)]
    pub fn drawn(&self) -> &str {
        &self.drawn
    }

    /// Read `body` (the text after the title) into the reader's rows, if it changed. Only the
    /// rows between the first and the last that differ are measured again, so ticking a box
    /// keeps the place the note is scrolled to.
    fn segment(&mut self, body: &str) {
        if body == self.segmented {
            return;
        }
        let new = crate::markdown::segments(body);
        let old = &self.segments;
        let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let (old_rest, new_rest) = (old.get(head..).unwrap_or(&[]), new.get(head..).unwrap_or(&[]));
        let tail =
            old_rest.iter().rev().zip(new_rest.iter().rev()).take_while(|(a, b)| a == b).count();
        let changed = old_rest.len().saturating_sub(tail);
        self.list.splice(head..head.saturating_add(changed), new_rest.len().saturating_sub(tail));
        self.segments = Rc::from(new);
        body.clone_into(&mut self.segmented);
    }

    /// The `ix`-th row of the reader: its segment, spaced from the one above as the column
    /// spaces the title from the first.
    fn render_segment(&self, ix: usize, cx: &Context<Self>) -> AnyElement {
        let Some(segment) = self.segments.get(ix) else { return div().into_any_element() };
        let id = self.id.as_uuid();
        let mono = self.theme.typography.mono_families.first().cloned().unwrap_or_default();
        let row = match segment {
            Segment::Prose(prose) => TextView::markdown(
                SharedString::from(format!("note-md-{id}-{ix}")),
                SharedString::from(prose.clone()),
            )
            .style(crate::markdown::style(&self.theme, &mono, self.zoom))
            .selectable(false)
            .into_any_element(),
            Segment::Task(task) => {
                let note = cx.entity();
                let toggle: crate::markdown::Toggle =
                    Rc::new(move |ix, window, cx: &mut gpui::App| {
                        note.update(cx, |n, cx| n.toggle_task(ix, window, cx));
                    });
                crate::markdown::task_row(
                    format!("note-task-{id}-{}", task.ix),
                    task,
                    &self.theme,
                    &mono,
                    self.zoom,
                    Some(toggle),
                )
            }
            Segment::Code { lang, body } => {
                let run = self.can_run.then(|| -> crate::markdown::Run {
                    let note = cx.entity();
                    Rc::new(move |code: String, cx: &mut gpui::App| {
                        note.update(cx, |_n, cx| cx.emit(NoteViewEvent::Run(code)));
                    })
                });
                crate::markdown::code_block(
                    (format!("note-code-copy-{id}-{ix}"), format!("note-code-run-{id}-{ix}")),
                    lang,
                    body,
                    &self.theme,
                    self.zoom,
                    run,
                )
            }
        };
        let gap = if ix == 0 { 0.0 } else { self.theme.spacing.xs * self.zoom };
        div().pt(px(gap)).child(row).into_any_element()
    }

    /// The document's text for this note, as the registry has it now: taken at once while the
    /// note is read, or kept until the editor lets go of the keyboard, then taken if nothing
    /// was typed meanwhile (what was typed is committed over it: last writer wins).
    pub fn offer_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if text == self.synced {
            self.offered = None;
            return;
        }
        if self.editing(window, cx) {
            self.offered = Some(text.to_owned());
            return;
        }
        self.set_text(text, window, cx);
    }

    /// The editor let go of the keyboard: what was typed goes to the document, or, with
    /// nothing typed, the text another client wrote meanwhile comes in.
    fn blurred(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.live_text(cx) != self.synced;
        self.commit(cx);
        if let Some(text) = self.offered.take()
            && !typed
        {
            self.set_text(&text, window, cx);
        }
        cx.notify();
    }

    /// The document changed under us (another client edited the note).
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if text == self.synced {
            return;
        }
        text.clone_into(&mut self.synced);
        self.generation = self.generation.wrapping_add(1);
        self.text.update(cx, |state, cx| state.set_value(text.to_owned(), window, cx));
        cx.notify();
    }

    /// Put the caret in the editor, at the end of the text: the rendered Markdown has no
    /// offset to click into, so entering a note means carrying on where the writing stopped.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.text.update(cx, |state, cx| {
            let end = state.text().len();
            state.set_selected_range(end..end, cx);
            state.focus(window, cx);
        });
    }

    /// Tick or untick the `ix`-th task line (the box on a rendered task row was pressed):
    /// the text changes in place, without the editor opening, and goes to the document at
    /// once.
    pub fn toggle_task(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text.read(cx).value().to_string();
        let Some(flipped) = crate::markdown::toggle_task(&text, ix) else { return };
        self.generation = self.generation.wrapping_add(1);
        self.text.update(cx, |state, cx| state.set_value(flipped.clone(), window, cx));
        self.synced.clone_from(&flipped);
        cx.emit(NoteViewEvent::Commit(flipped));
        cx.notify();
    }

    fn schedule_commit(&mut self, cx: &Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COMMIT_AFTER).await;
            let _gone = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.commit(cx);
                }
            });
        })
        .detach();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        let value = self.text.read(cx).value().to_string();
        if value == self.synced {
            return;
        }
        self.synced.clone_from(&value);
        // What was typed is written over whatever another client wrote meanwhile.
        self.offered = None;
        cx.emit(NoteViewEvent::Commit(value));
    }
}

impl EventEmitter<NoteViewEvent> for NoteView {}

/// The note's title and what follows it, when its first line is one.
///
/// A heading, or a line of prose that opens the note, is a title. A task, a list item, a quote,
/// a table row or a fence opens the body instead, and the note has no title.
#[must_use]
pub fn split_title(text: &str) -> Option<(&str, &str)> {
    let lead = text.len().saturating_sub(text.trim_start().len());
    let rest = text.get(lead..)?;
    let (line, after) = rest.split_once('\n').unwrap_or((rest, ""));
    let line = line.trim_end();
    let body = ["- ", "* ", "+ ", ">", "|", "```", "~~~"].iter().any(|m| line.starts_with(m))
        || line
            .split_once(". ")
            .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if body {
        return None;
    }
    let title = line.trim_start_matches('#').trim();
    (!title.is_empty()).then_some((title, after))
}

impl Focusable for NoteView {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.text.read(cx).focus_handle(cx)
    }
}

impl Render for NoteView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.text.read(cx).value().to_string();
        #[cfg(test)]
        {
            self.renders = self.renders.saturating_add(1);
            self.drawn.clone_from(&text);
        }
        // An empty note has nothing to render but the placeholder, which is the editor's own.
        let reading = !self.editing(window, cx) && !text.is_empty();
        let body = div().size_full().p(px(self.pad * self.zoom));
        if reading {
            // The first line, when it is a title, leads at the title size in the strong
            // weight: the same size and weight as its items, it read as one more of them.
            let (title, rest) =
                split_title(&text).map_or((None, text.as_str()), |(t, r)| (Some(t), r));
            self.segment(rest);
            let id = self.id.as_uuid();
            let title = title.map(|title| {
                div()
                    .debug_selector(move || format!("note-title-{id}"))
                    .text_size(px(self.theme.typography.title() * self.zoom))
                    .font_weight(gpui::FontWeight(slopty_theme::Typography::STRONG_WEIGHT))
                    .text_color(crate::colors::hsla(self.theme.surfaces.text))
                    .child(SharedString::from(title.to_owned()))
            });
            // Fenced blocks are their own rows, with copy and run buttons: a note of commands
            // is a runbook.
            let rows = list(
                self.list.clone(),
                cx.processor(|this, ix: usize, _window, cx| this.render_segment(ix, cx)),
            )
            .size_full();
            body.id(SharedString::from(format!("note-read-{id}")))
                .debug_selector(|| format!("note-read-{id}"))
                // gpui-kit draws Markdown as styled text, which leaves nothing in the
                // accessibility tree: the note reads itself out, as its editor does.
                .role(Role::Document)
                .aria_label("Note")
                .aria_value(SharedString::from(text.clone()))
                .overflow_hidden()
                .cursor_text()
                .text_size(px(self.text_size * self.zoom))
                .line_height(gpui::relative(self.theme.typography.markdown_line_height))
                // A click reads as "edit this": the caret goes in and the editor takes over.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, window, cx| {
                        this.focus(window, cx);
                        cx.notify();
                    }),
                )
                .flex()
                .flex_col()
                .gap(px(self.theme.spacing.xs * self.zoom))
                .children(title)
                .child(div().flex_1().min_h_0().w_full().child(rows))
                .into_any_element()
        } else {
            // The field pads itself by its size's inset; the body gives up that much, so the
            // caret starts where the rendered note's text did and the header's title does.
            let pad = px(self.pad * self.zoom);
            body.px((pad - Size::Small.input_px()).max(px(0.0)))
                .py((pad - Size::Small.input_py()).max(px(0.0)))
                .text_size(px(self.text_size * self.zoom))
                .child(
                    Textarea::new(&self.text)
                        .small()
                        .appearance(false)
                        .bordered(false)
                        .aria_label("Note")
                        .h(gpui::relative(1.0)),
                )
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, point, px, size,
    };
    use slopty_core::ItemId;
    use slopty_theme::Theme;

    use super::{NoteView, split_title};

    /// A note of 2 000 tasks draws only the rows in view: the first box is drawn and the last
    /// is not. A scroll brings later rows in and takes the first out, and a box ticked down
    /// there ticks its own line and leaves the note where it was scrolled to.
    #[gpui::test]
    fn a_long_note_draws_the_rows_in_view_and_scrolls(cx: &mut TestAppContext) {
        const TASKS: usize = 2000;
        cx.update(gpui_kit::init);
        let text = (0..TASKS).fold(String::new(), |mut text, n| {
            text.push_str("- [ ] task ");
            text.push_str(&n.to_string());
            text.push('\n');
            text
        });
        let id = ItemId::new();
        let (note, cx) =
            cx.add_window_view(|window, cx| NoteView::new(id, &text, Theme::default(), window, cx));
        cx.simulate_resize(size(px(400.0), px(300.0)));
        cx.run_until_parked();
        let task = |ix: usize| -> &'static str {
            Box::leak(format!("note-task-{}-{ix}", id.as_uuid()).into_boxed_str())
        };
        let drawn = |cx: &mut gpui::VisualTestContext| -> Vec<usize> {
            (0..TASKS).filter(|ix| cx.debug_bounds(task(*ix)).is_some()).collect()
        };
        let first = drawn(cx);
        assert_eq!(first.first(), Some(&0), "the first row is drawn");
        assert!(first.len() < 40, "only the rows in view: {first:?}");

        cx.simulate_event(ScrollWheelEvent {
            position: point(px(200.0), px(150.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-3000.0))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();
        let later = drawn(cx);
        let Some(&at) = later.get(later.len() / 2) else { panic!("rows after a scroll") };
        let past = first.last().copied().unwrap_or_default();
        assert!(!later.contains(&0) && at > past, "a scroll brings later rows in: {later:?}");

        let bounds = cx.debug_bounds(task(at)).expect("the box in view");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        let ticked = format!("- [x] task {at}\n");
        assert!(note.read_with(cx, |n, _| n.synced().contains(&ticked)), "its own line ticks");
        assert_eq!(cx.debug_bounds(task(at)), Some(bounds), "and the note stays where it was");
    }

    /// A heading or a line of prose opening a note is its title, blank lines before it
    /// skipped; a note that opens on a task, a list, a quote or a fence has none.
    #[test]
    fn the_first_line_is_a_title_unless_it_opens_the_body() {
        assert_eq!(split_title("Release\n\n- [x] build"), Some(("Release", "\n- [x] build")));
        assert_eq!(split_title("\n  # Plan  \nmore"), Some(("Plan", "more")));
        assert_eq!(split_title("Alone"), Some(("Alone", "")));
        for body in ["- [ ] ship", "* item", "> quoted", "1. first", "```sh\nls\n```", "#", ""] {
            assert_eq!(split_title(body), None, "{body:?}");
        }
    }
}
