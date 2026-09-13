//! A sticky note on the canvas: Markdown read, free text edited in place, stored in the
//! shared document.
//!
//! The text lives in the canvas item (`ItemKind::Note`), so every client sees it. A note that
//! is not being edited draws its text as Markdown ([`crate::markdown::style`], the tokens the
//! conversation reads by); a click or the canvas putting the caret in it swaps in the editor,
//! and blur swaps back. Edits are committed to the host after a short pause in typing and on
//! blur; a remote change is taken only while this client is not editing (last writer wins, no
//! merge).

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::TextView;
use slopty_core::ItemId;
use slopty_theme::Theme;

/// Typing pause before the text is sent to the host.
const COMMIT_AFTER: Duration = Duration::from_millis(400);

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
            TextareaState::new(window, cx).placeholder("write…").default_value(text.to_owned())
        });
        let subscription = cx.subscribe(&state, |this, _state, event, cx| match event {
            InputEvent::Change => this.schedule_commit(cx),
            // Focus and blur swap the reader for the editor and back.
            InputEvent::Blur => {
                this.commit(cx);
                cx.notify();
            }
            InputEvent::Focus => cx.notify(),
            InputEvent::PressEnter { .. } => {}
        });
        Self {
            id,
            text: state,
            synced: text.to_owned(),
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

    /// Whether the editor has keyboard focus.
    #[must_use]
    pub fn editing(&self, window: &Window, cx: &gpui::App) -> bool {
        self.text.read(cx).focus_handle(cx).is_focused(window)
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
                // Fenced blocks are their own elements, with the conversation's copy and run
                // buttons: a note of commands is a runbook.
                .children(crate::markdown::segments(&text).into_iter().enumerate().map(
                    |(si, segment)| match segment {
                        crate::markdown::Segment::Prose(prose) => TextView::markdown(
                            SharedString::from(format!("note-md-{id}-{si}")),
                            SharedString::from(prose),
                        )
                        .style(crate::markdown::style(&self.theme, &mono, self.zoom))
                        .selectable(false)
                        .into_any_element(),
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
            body.text_size(px(self.text_size * self.zoom))
                .child(
                    Textarea::new(&self.text)
                        .appearance(false)
                        .bordered(false)
                        .aria_label("Note")
                        .h(gpui::relative(1.0)),
                )
                .into_any_element()
        }
    }
}
