//! A sticky note on the canvas: Markdown read, free text edited in place, stored in the
//! shared document.
//!
//! The text lives in the canvas item (`ItemKind::Note`), so every client sees it. A note that
//! is not being edited draws its text as Markdown ([`crate::markdown::style`]); a click or the
//! canvas putting the caret in it swaps in the editor, and blur swaps back. Edits are committed to
//! the worker after a short pause in typing and on blur; a remote change is taken only while this
//! client is not editing (last writer wins, no merge). A task line (`- [ ] …`) is read as a row
//! whose box ticks on a click, the one edit that needs no editor.

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Sizable as _, Size};
use slopty_core::ItemId;
use slopty_theme::Theme;

/// Typing pause before the text is sent to the worker.
const COMMIT_AFTER: Duration = Duration::from_millis(400);

/// What an empty note says.
pub(crate) const WRITE_PLACEHOLDER: &str = "Write…";

/// What a note tells the canvas.
#[derive(Clone, Debug)]
pub enum NoteViewEvent {
    /// The text changed and should be written to the document.
    Commit(String),
    /// A fenced block's "run" button: type its code into the canvas's shell.
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
    /// The canvas has a shell to run a fenced block in (the "run" button shows only then).
    can_run: bool,
    theme: Theme,
    _subscription: gpui::Subscription,
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
            _subscription: subscription,
        }
    }

    /// Item this note belongs to.
    #[must_use]
    pub const fn id(&self) -> ItemId {
        self.id
    }

    /// Whether the canvas has a shell to run a fenced block in: with one, a block that is
    /// read (not edited) carries a "run" button beside its "copy".
    pub fn set_can_run(&mut self, can: bool, cx: &mut Context<Self>) {
        if self.can_run != can {
            self.can_run = can;
            cx.notify();
        }
    }

    /// The text as last committed or received.
    #[must_use]
    pub fn synced(&self) -> &str {
        &self.synced
    }

    /// What the field holds this instant, keystrokes the commit timer has not carried into
    /// the document included. The canvas reads this when it has to act on the note's text
    /// before that timer fires — closing the card, which must take back what was typed.
    #[must_use]
    pub fn live_text(&self, cx: &gpui::App) -> String {
        self.text.read(cx).value().to_string()
    }

    /// Whether the editor has keyboard focus.
    #[must_use]
    pub fn editing(&self, window: &Window, cx: &gpui::App) -> bool {
        self.text.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Paint scale (the canvas's zoom) and the theme's inset and type size at scale 1. The
    /// note is drawn from a cached view, so a change here draws it afresh.
    pub fn set_layout(&mut self, zoom: f32, pad: f32, text_size: f32, cx: &mut Context<Self>) {
        let changed = [(self.zoom, zoom), (self.pad, pad), (self.text_size, text_size)]
            .iter()
            .any(|(was, now)| (was - now).abs() > f32::EPSILON);
        if changed {
            self.zoom = zoom;
            self.pad = pad;
            self.text_size = text_size;
            cx.notify();
        }
    }

    /// Draw by another theme (the canvas swapped it).
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme == theme {
            return;
        }
        self.theme = theme;
        cx.notify();
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

impl Focusable for NoteView {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.text.read(cx).focus_handle(cx)
    }
}

impl Render for NoteView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.text.read(cx).value().to_string();
        // An empty note has nothing to render but the placeholder, which is the editor's own.
        let reading = !self.editing(window, cx) && !text.is_empty();
        let mono = self.theme.typography.mono_families.first().cloned().unwrap_or_default();
        let body = div().size_full().p(px(self.pad * self.zoom));
        if reading {
            let id = self.id.as_uuid();
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
                // Fenced blocks are their own elements, with copy and run buttons: a note of
                // commands is a runbook.
                .children(crate::markdown::segments(&text).into_iter().enumerate().map(
                    |(si, segment)| match segment {
                        crate::markdown::Segment::Prose(prose) => TextView::markdown(
                            SharedString::from(format!("note-md-{id}-{si}")),
                            SharedString::from(prose),
                        )
                        .style(crate::markdown::style(&self.theme, &mono, self.zoom))
                        .selectable(false)
                        .into_any_element(),
                        crate::markdown::Segment::Task(task) => {
                            let note = cx.entity();
                            let toggle: crate::markdown::Toggle =
                                std::rc::Rc::new(move |ix, window, cx: &mut gpui::App| {
                                    note.update(cx, |n, cx| n.toggle_task(ix, window, cx));
                                });
                            crate::markdown::task_row(
                                format!("note-task-{id}-{}", task.ix),
                                &task,
                                &self.theme,
                                &mono,
                                self.zoom,
                                Some(toggle),
                            )
                        }
                        crate::markdown::Segment::Code { lang, body } => {
                            let run = self.can_run.then(|| -> crate::markdown::Run {
                                let note = cx.entity();
                                std::rc::Rc::new(move |code: String, cx: &mut gpui::App| {
                                    note.update(cx, |_n, cx| cx.emit(NoteViewEvent::Run(code)));
                                })
                            });
                            crate::markdown::code_block(
                                (format!("note-code-copy-{id}-{si}"), format!("note-code-run-{id}-{si}")),
                                &lang,
                                &body,
                                &self.theme,
                                self.zoom,
                                run,
                            )
                        }
                    },
                ))
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
