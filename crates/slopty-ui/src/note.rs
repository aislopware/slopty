//! A sticky note on the canvas: free text edited in place, stored in the shared document.
//!
//! The text lives in the canvas item (`ItemKind::Note`), so every client sees it. Edits are
//! committed to the host after a short pause in typing and on blur; a remote change is taken
//! only while this client is not editing (last writer wins, no merge).

use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement as _, Render, Styled as _, Window, div, px,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use slopty_core::ItemId;

/// Typing pause before the text is sent to the host.
const COMMIT_AFTER: Duration = Duration::from_millis(400);

/// What a note tells the canvas.
#[derive(Clone, Debug)]
pub enum NoteViewEvent {
    /// The text changed and should be written to the document.
    Commit(String),
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
    pub fn new(id: ItemId, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| {
            TextareaState::new(window, cx).placeholder("write…").default_value(text.to_owned())
        });
        let subscription = cx.subscribe(&state, |this, _state, event, cx| match event {
            InputEvent::Change => this.schedule_commit(cx),
            InputEvent::Blur => this.commit(cx),
            InputEvent::Focus | InputEvent::PressEnter { .. } => {}
        });
        Self {
            id,
            text: state,
            synced: text.to_owned(),
            generation: 0,
            zoom: 1.0,
            pad: 8.0,
            text_size: 13.0,
            _subscription: subscription,
        }
    }

    /// Item this note belongs to.
    #[must_use]
    pub const fn id(&self) -> ItemId {
        self.id
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

    /// Put the caret in the editor.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.text.update(cx, |state, cx| state.focus(window, cx));
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
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p(px(self.pad * self.zoom))
            .text_size(px(self.text_size * self.zoom))
            .child(
                Textarea::new(&self.text)
                    .appearance(false)
                    .bordered(false)
                    .aria_label("Note")
                    .h(gpui::relative(1.0)),
            )
    }
}
