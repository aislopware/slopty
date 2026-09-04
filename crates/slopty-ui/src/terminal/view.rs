//! `TerminalView`: one attached session on screen.

use std::time::{Duration, Instant};

use gpui::{
    Autocapitalize, Bounds, Context, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement as _, Pixels, Render, ScrollDelta, ScrollWheelEvent, Styled as _,
    TextInputAction, TextInputConfiguration, UTF16Selection, Window, div, point, size,
};
use slopty_client::{Effect, TermState};
use slopty_core::SessionId;
use slopty_grid::Cursor;
use slopty_predict::{Policy, Prediction, Predictor};
use slopty_proto::ClientMsg;
use slopty_proto::input::{MouseAction, MouseButton as ProtoButton, MouseEvent};
use slopty_proto::terminal::{TermEvent, TermRequest, TermSize};
use slopty_theme::Theme;
use tokio::sync::mpsc;

use crate::keys;
use crate::terminal::element::{CellMetrics, TerminalElement};

/// Things the surrounding UI may want to react to.
#[derive(Clone, Debug)]
pub enum TerminalViewEvent {
    /// Title changed.
    Title(String),
    /// Bell.
    Bell,
    /// The child exited.
    Exited(i32),
}

/// One session's view.
pub struct TerminalView {
    session: SessionId,
    state: TermState,
    out: mpsc::Sender<ClientMsg>,
    focus: FocusHandle,
    theme: Theme,
    key_seq: u64,
    metrics: Option<CellMetrics>,
    pending_size: Option<TermSize>,
    font_family: Option<String>,
    zoom: f32,
    predictor: Predictor,
}

impl std::fmt::Debug for TerminalView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalView").field("session", &self.session).finish_non_exhaustive()
    }
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

/// `SLOPTY_PREDICT=never|adaptive|always` overrides the local-echo policy (testing on fast links).
fn policy_from_env() -> Policy {
    match std::env::var("SLOPTY_PREDICT").as_deref() {
        Ok("never") => Policy::Never,
        Ok("always") => Policy::Always,
        _ => Policy::Adaptive,
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalView {
    /// A view for `session`; `out` is the link's sender.
    pub fn new(
        session: SessionId,
        size: TermSize,
        out: mpsc::Sender<ClientMsg>,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Self {
        Self {
            session,
            state: TermState::new(size),
            out,
            focus: cx.focus_handle(),
            theme,
            key_seq: 0,
            metrics: None,
            pending_size: None,
            font_family: None,
            zoom: 1.0,
            predictor: Predictor::new(policy_from_env()),
        }
    }

    /// Session id.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// The state (for the element).
    #[must_use]
    pub const fn state(&self) -> &TermState {
        &self.state
    }

    /// The theme (for the element).
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        &self.theme
    }

    /// The monospace family the element resolved (first installed from the theme's list).
    #[must_use]
    pub fn font_family(&self) -> Option<&str> {
        self.font_family.as_deref()
    }

    /// Record the resolved family.
    pub fn set_font_family(&mut self, family: String) {
        self.font_family = Some(family);
    }

    /// Paint scale (set by the canvas before each frame).
    pub const fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom;
    }

    /// Link RTT, for the prediction policy.
    pub const fn set_rtt(&mut self, rtt: Option<Duration>) {
        self.predictor.set_rtt(rtt);
    }

    /// Predicted cells to overlay and the cursor to draw, when prediction is showing.
    #[must_use]
    pub fn predictions(&self) -> Option<(Vec<Prediction>, Cursor)> {
        if !self.predictor.visible(Instant::now()) || self.state.view_offset() != 0 {
            return None;
        }
        let pending: Vec<Prediction> = self.predictor.pending().iter().cloned().collect();
        Some((pending, self.predictor.cursor(self.state.cursor())))
    }

    /// Lifetime prediction hits and misses.
    #[must_use]
    pub const fn prediction_stats(&self) -> (u64, u64) {
        self.predictor.stats()
    }

    /// Cell metrics measured by the element on its last layout.
    #[must_use]
    pub const fn metrics(&self) -> Option<CellMetrics> {
        self.metrics
    }

    /// Feed an event from the link.
    pub fn apply(&mut self, event: TermEvent, cx: &mut Context<Self>) {
        let reconcile = matches!(event, TermEvent::Frame(_));
        if matches!(event, TermEvent::Resized { .. }) {
            self.predictor.flush();
        }
        let effects = self.state.apply(event);
        if reconcile {
            let outcome = self.predictor.on_frame(
                self.state.screen(),
                self.state.input_ack(),
                self.state.epoch().unwrap_or(0),
                Instant::now(),
            );
            if outcome.misses > 0 {
                tracing::debug!(session = %self.session, ?outcome, "prediction miss");
            }
        }
        for effect in effects {
            match effect {
                Effect::Request(req) => self.send(req),
                Effect::Title(t) => cx.emit(TerminalViewEvent::Title(t)),
                Effect::Bell => cx.emit(TerminalViewEvent::Bell),
                Effect::Exited(status) => cx.emit(TerminalViewEvent::Exited(status)),
                Effect::ClipboardWrite(text) => {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                }
                Effect::ClipboardReadRequest => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                        self.send(TermRequest::ClipboardRead { text });
                    }
                }
                Effect::Cwd(_) => {}
                Effect::Error(e) => tracing::warn!(session = %self.session, error = %e, "host"),
            }
        }
        cx.notify();
    }

    /// The element measured the grid: `cols × rows` fit, with these metrics.
    pub fn fitted(&mut self, size: TermSize, metrics: CellMetrics, cx: &mut Context<Self>) {
        self.metrics = Some(metrics);
        if self.state.size() == size || self.pending_size == Some(size) {
            return;
        }
        self.pending_size = Some(size);
        for effect in self.state.resize(size) {
            if let Effect::Request(req) = effect {
                self.send(req);
            }
        }
        cx.notify();
    }

    fn send(&self, req: TermRequest) {
        let msg = ClientMsg::Term { session: self.session, req };
        if let Err(e) = self.out.try_send(msg) {
            tracing::warn!(session = %self.session, error = %e, "outbound queue");
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // Cmd shortcuts belong to the app.
        if event.keystroke.modifiers.platform {
            return;
        }
        self.key_seq = self.key_seq.wrapping_add(1);
        let key = keys::key_event(self.key_seq, &event.keystroke, event.is_held);
        tracing::trace!(session = %self.session, ?key, "key");
        if self.state.view_offset() != 0 {
            self.state.scroll_to_bottom();
        }
        let _guess = self.predictor.on_key(
            &key,
            self.state.cursor(),
            self.state.size().cols,
            self.state.modes(),
            Instant::now(),
        );
        self.send(TermRequest::Key(key));
        cx.stop_propagation();
        cx.notify();
    }

    fn scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lines = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => {
                let line_height = self.metrics.map_or(20.0, |m| f32::from(m.line_height));
                f32::from(p.y) / line_height
            }
        };
        // Wheel up (positive y in GPUI) scrolls into history.
        #[expect(clippy::cast_possible_truncation, reason = "whole lines")]
        let delta = lines.round() as i64;
        if delta == 0 {
            return;
        }
        for effect in self.state.scroll(delta) {
            if let Effect::Request(req) = effect {
                self.send(req);
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        let Some((col, row)) = self.metrics.and_then(|m| m.cell_at(event.position)) else {
            return;
        };
        let button = match event.button {
            MouseButton::Left => ProtoButton::Left,
            MouseButton::Right => ProtoButton::Right,
            MouseButton::Middle => ProtoButton::Middle,
            MouseButton::Navigate(_) => return,
        };
        let (px, py) = self.metrics.map_or((0, 0), |m| m.pixel_at(event.position));
        self.send(TermRequest::Mouse(MouseEvent {
            action: MouseAction::Press,
            button: Some(button),
            mods: keys::mods(event.modifiers),
            col,
            row,
            px,
            py,
        }));
    }
}

/// Text input on top of the key path.
///
/// Keys reach the terminal through [`TerminalView::key_down`]; this handler exists so the
/// platform treats a focused terminal as a text field: iOS raises the soft keyboard (typed
/// characters still arrive as key events), and macOS input methods commit composed text here.
/// The terminal has no editable buffer, so ranges are empty and composition is not previewed.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            return;
        }
        if self.state.view_offset() != 0 {
            self.state.scroll_to_bottom();
        }
        self.send(TermRequest::Raw(text.as_bytes().to_vec()));
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        _new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The cursor cell: where an input method should hang its candidate window.
        let m = self.metrics?;
        let cursor = self.state.cursor();
        let origin = point(
            m.origin.x + m.cell_width * f32::from(cursor.col),
            m.origin.y + m.line_height * f32::from(cursor.row),
        );
        Some(Bounds::new(origin, size(m.cell_width, m.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn text_input_configuration(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> TextInputConfiguration {
        TextInputConfiguration {
            autocorrect: false,
            autocapitalize: Autocapitalize::None,
            suggestions: false,
            input_action: TextInputAction::Enter,
        }
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        div()
            .id("terminal")
            .track_focus(&self.focus)
            .size_full()
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .child(TerminalElement::new(cx.entity(), focused).zoom(self.zoom))
    }
}
