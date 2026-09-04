//! `ScreenView`: one remote window or display, painted from the newest decoded frame.
//!
//! The frame arrives as an IOSurface-backed `CVPixelBuffer` from `slopty-codec`; GPUI's
//! `surface` element samples it through `CVMetalTextureCache`, so nothing is copied on the
//! client. The host's cursor is drawn here from the cursor channel (one RTT behind the pointer,
//! not one video pipeline). Pointer, scroll and key events inside the view go to the host as
//! `ScreenInput` in stream pixels; the host injects them. ⌘ chords the canvas binds (⌘T/⌘O/⌘W,
//! zoom) never reach the view because GPUI runs key bindings before key listeners; every other
//! chord (⌘C, ⌘V, ⌘Z, ⌘S…) is forwarded to the remote window. The view also asks the host for a
//! smaller stream when it is painted small (canvas zoomed out), quantised so the encoder is
//! not rebuilt on every wheel tick.

use std::sync::Arc;
use std::time::{Duration, Instant};

use core_foundation::base::TCFType as _;
use core_video::pixel_buffer::CVPixelBuffer;
use gpui::{
    Bounds, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit,
    ParentElement as _, PathBuilder, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent,
    Styled as _, Task, TouchPhase, Window, canvas, div, point, px, surface,
};
use slopty_client::{CursorState, ScreenHandle};
use slopty_codec::DecodedFrame;
use slopty_core::StreamId;
use slopty_proto::ClientMsg;
use slopty_proto::input::{KeyAction, KeyCode, MouseButton as ProtoButton};
use slopty_proto::screen::{
    CaptureTarget, Quality, ScreenInput, ScreenRequest, ScrollPhase, VideoCodec,
};
use slopty_theme::Theme;
use tokio::sync::mpsc;

use crate::colors::hsla;
use crate::keys;

/// Makes a client-side stream for an `Opened` event (wraps `HostLink::screen`).
pub type ScreenFactory = Arc<dyn Fn(StreamId, VideoCodec) -> ScreenHandle + Send + Sync>;

/// Quality change rate limit.
const QUALITY_COOLDOWN: Duration = Duration::from_millis(400);
/// Smallest scale the view asks for.
const MIN_SCALE: f32 = 0.25;

/// Things the canvas may react to.
#[derive(Clone, Copy, Debug)]
pub enum ScreenViewEvent {
    /// First frame painted.
    Ready,
}

/// What the host answered to `Open`, plus what we asked for.
#[derive(Clone, Copy, Debug)]
pub struct Opened {
    /// Stream id.
    pub stream: StreamId,
    /// Target.
    pub target: CaptureTarget,
    /// Stream pixel size.
    pub size: (u32, u32),
    /// Requested quality.
    pub quality: Quality,
}

/// One stream on screen.
pub struct ScreenView {
    stream: StreamId,
    target: CaptureTarget,
    handle: ScreenHandle,
    /// Newest frame and its Metal-ready wrapper.
    latest: Option<(Arc<DecodedFrame>, CVPixelBuffer)>,
    /// Stream size in pixels as opened.
    size: (u32, u32),
    /// Native pixel size of the target (stream size at scale 1).
    native: (f32, f32),
    quality: Quality,
    quality_changed: Instant,
    cursor: CursorState,
    out: mpsc::Sender<ClientMsg>,
    theme: Theme,
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
    frames: u64,
    /// Keys whose press went to the host, so a release for a locally-handled chord (its press
    /// was eaten by a canvas binding) is not forwarded as a stray key-up.
    held: Vec<KeyCode>,
    _pump: Task<()>,
}

impl std::fmt::Debug for ScreenView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenView")
            .field("stream", &self.stream)
            .field("target", &self.target)
            .field("size", &self.size)
            .field("frames", &self.frames)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<ScreenViewEvent> for ScreenView {}

impl Focusable for ScreenView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ScreenView {
    /// Wrap an opened stream.
    pub fn new(
        opened: Opened,
        handle: ScreenHandle,
        out: mpsc::Sender<ClientMsg>,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Self {
        let Opened { stream, target, size, quality } = opened;
        let mut frames = handle.frames();
        let mut cursor = handle.cursor();
        let pump = cx.spawn(async move |this, cx| {
            loop {
                tokio::select! {
                    changed = frames.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let frame = frames.borrow_and_update().clone();
                        let alive = this.update(cx, |view, cx| {
                            view.take_frame(frame, cx);
                            cx.notify();
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                    changed = cursor.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let state = *cursor.borrow_and_update();
                        let alive = this.update(cx, |view, cx| {
                            view.cursor = state;
                            cx.notify();
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let scale = quality.scale.clamp(MIN_SCALE, 1.0);
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let native = (size.0 as f32 / scale, size.1 as f32 / scale);
        Self {
            stream,
            target,
            handle,
            latest: None,
            size,
            native,
            quality,
            quality_changed: Instant::now(),
            cursor: CursorState::default(),
            out,
            theme,
            focus: cx.focus_handle(),
            bounds: Bounds::default(),
            frames: 0,
            held: Vec::new(),
            _pump: pump,
        }
    }

    /// Stream id.
    #[must_use]
    pub const fn stream(&self) -> StreamId {
        self.stream
    }

    /// What is streamed.
    #[must_use]
    pub const fn target(&self) -> CaptureTarget {
        self.target
    }

    /// Frames painted so far.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// Receiver counters.
    #[must_use]
    pub fn stats(&self) -> slopty_client::ScreenStats {
        self.handle.stats()
    }

    /// The canvas reports how wide the view is painted (device pixels) so the stream can be
    /// downscaled at the host when zoomed out. Quantised to quarter steps and rate limited.
    pub fn set_painted_width(&mut self, device_px: f32) {
        let wanted = (device_px / self.native.0).clamp(MIN_SCALE, 1.0);
        let bucket = (wanted * 4.0).ceil() / 4.0;
        if (bucket - self.quality.scale).abs() < f32::EPSILON
            || self.quality_changed.elapsed() < QUALITY_COOLDOWN
        {
            return;
        }
        self.quality.scale = bucket;
        self.quality_changed = Instant::now();
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
        let side = |native: f32| ((native * bucket).round().max(2.0) as u32).next_multiple_of(2);
        self.size = (side(self.native.0), side(self.native.1));
        self.send(ScreenRequest::SetQuality { stream: self.stream, quality: self.quality });
    }

    /// The host resized the target: the stream now has this pixel size at the current quality
    /// scale, so the native size follows from it.
    pub fn set_geometry(&mut self, width: u32, height: u32) {
        self.size = (width, height);
        let scale = self.quality.scale.clamp(MIN_SCALE, 1.0);
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let native = (width as f32 / scale, height as f32 / scale);
        self.native = native;
    }

    /// Native size of the target in stream pixels at scale 1.
    #[must_use]
    pub const fn native(&self) -> (f32, f32) {
        self.native
    }

    fn take_frame(&mut self, frame: Option<Arc<DecodedFrame>>, cx: &mut Context<Self>) {
        let Some(frame) = frame else { return };
        let raw = std::ptr::from_ref(frame.image.as_cv())
            .cast_mut()
            .cast::<core_video::buffer::__CVBuffer>();
        // SAFETY: `raw` is a live `CVPixelBufferRef` owned by `frame`; `wrap_under_get_rule`
        // takes its own retain, so the wrapper stays valid even if `frame` is dropped first.
        let buffer = unsafe { CVPixelBuffer::wrap_under_get_rule(raw) };
        #[expect(clippy::cast_possible_truncation, reason = "pixel counts")]
        let size = (frame.image.width() as u32, frame.image.height() as u32);
        self.size = size;
        self.latest = Some((frame, buffer));
        self.frames = self.frames.saturating_add(1);
        if self.frames == 1 {
            cx.emit(ScreenViewEvent::Ready);
        }
    }

    fn send(&self, req: ScreenRequest) {
        if let Err(e) = self.out.try_send(ClientMsg::Screen(req)) {
            tracing::warn!(stream = %self.stream, error = %e, "outbound queue");
        }
    }

    fn input(&self, input: ScreenInput) {
        self.send(ScreenRequest::Input { stream: self.stream, input });
    }

    /// Window position → stream pixels.
    fn to_stream(&self, position: Point<Pixels>) -> (f32, f32) {
        let width = f32::from(self.bounds.size.width).max(1.0);
        let height = f32::from(self.bounds.size.height).max(1.0);
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let (stream_w, stream_h) = (self.size.0 as f32, self.size.1 as f32);
        let x = (f32::from(position.x) - f32::from(self.bounds.origin.x)) / width * stream_w;
        let y = (f32::from(position.y) - f32::from(self.bounds.origin.y)) / height * stream_h;
        tracing::trace!(?position, bounds = ?self.bounds, size = ?self.size, x, y, "to_stream");
        (x, y)
    }

    fn inside(&self, p: Point<Pixels>) -> bool {
        self.bounds.contains(&p)
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _w: &mut Window, _cx: &mut Context<Self>) {
        if !self.inside(ev.position) {
            return;
        }
        let (x, y) = self.to_stream(ev.position);
        self.input(ScreenInput::Move { x, y });
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            self.send(ScreenRequest::Focus(self.stream));
        }
        self.focus.focus(window, cx);
        let button = proto_button(ev.button);
        let (x, y) = self.to_stream(ev.position);
        self.input(ScreenInput::Button {
            button,
            down: true,
            x,
            y,
            clicks: u8::try_from(ev.click_count).unwrap_or(u8::MAX),
            mods: keys::mods(ev.modifiers),
        });
        cx.stop_propagation();
    }

    fn mouse_up(&mut self, ev: &MouseUpEvent, _w: &mut Window, _cx: &mut Context<Self>) {
        let button = proto_button(ev.button);
        let (x, y) = self.to_stream(ev.position);
        self.input(ScreenInput::Button {
            button,
            down: false,
            x,
            y,
            clicks: u8::try_from(ev.click_count).unwrap_or(u8::MAX),
            mods: keys::mods(ev.modifiers),
        });
    }

    fn scroll_wheel(&mut self, ev: &ScrollWheelEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if ev.modifiers.platform {
            // ⌘-scroll is the canvas zoom gesture; let it through.
            return;
        }
        let (dx, dy, precise) = match ev.delta {
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y), true),
            ScrollDelta::Lines(l) => (l.x, l.y, false),
        };
        let phase = match ev.touch_phase {
            TouchPhase::Started => ScrollPhase::Began,
            TouchPhase::Moved => ScrollPhase::Changed,
            TouchPhase::Ended => ScrollPhase::Ended,
            TouchPhase::Cancelled => ScrollPhase::Cancelled,
        };
        let (x, y) = self.to_stream(ev.position);
        self.input(ScreenInput::Scroll {
            dx,
            dy,
            precise,
            phase,
            momentum: ScrollPhase::None,
            x,
            y,
            mods: keys::mods(ev.modifiers),
        });
        cx.stop_propagation();
    }

    fn key_down(&mut self, ev: &KeyDownEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let code = keys::key_code(&ev.keystroke.key);
        if !self.held.contains(&code) {
            self.held.push(code);
        }
        self.input(ScreenInput::Key {
            code,
            action: if ev.is_held { KeyAction::Repeat } else { KeyAction::Press },
            mods: keys::mods(ev.keystroke.modifiers),
            text: ev.keystroke.key_char.clone().filter(|t| !t.is_empty()),
        });
        cx.stop_propagation();
    }

    fn key_up(&mut self, ev: &KeyUpEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let code = keys::key_code(&ev.keystroke.key);
        let Some(at) = self.held.iter().position(|&c| c == code) else { return };
        self.held.swap_remove(at);
        self.input(ScreenInput::Key {
            code,
            action: KeyAction::Release,
            mods: keys::mods(ev.keystroke.modifiers),
            text: None,
        });
        cx.stop_propagation();
    }

    /// The host's pointer as an arrow, in view coordinates.
    fn cursor_overlay(&self) -> Option<impl IntoElement + use<>> {
        if !self.cursor.visible || self.latest.is_none() {
            return None;
        }
        let w = f32::from(self.bounds.size.width).max(1.0);
        let h = f32::from(self.bounds.size.height).max(1.0);
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let (sw, sh) = (self.size.0.max(1) as f32, self.size.1.max(1) as f32);
        #[expect(clippy::cast_precision_loss, reason = "cursor coordinates are small")]
        let (cx_px, cy_px) = (self.cursor.x as f32 / sw * w, self.cursor.y as f32 / sh * h);
        let fill = hsla(self.theme.surfaces.text);
        let outline = hsla(self.theme.surfaces.canvas);
        Some(
            canvas(
                |_bounds, _window, _cx| {},
                move |bounds, (), window, _cx| {
                    let origin = point(bounds.origin.x + px(cx_px), bounds.origin.y + px(cy_px));
                    let arrow = |inset: f32, scale: f32| {
                        let mut path = PathBuilder::fill();
                        let at = |x: f32, y: f32| {
                            point(
                                origin.x + px(x.mul_add(scale, inset)),
                                origin.y + px(y.mul_add(scale, inset)),
                            )
                        };
                        path.move_to(at(0.0, 0.0));
                        path.line_to(at(0.0, 16.0));
                        path.line_to(at(4.0, 12.5));
                        path.line_to(at(7.0, 18.5));
                        path.line_to(at(9.5, 17.5));
                        path.line_to(at(6.5, 11.5));
                        path.line_to(at(11.5, 11.5));
                        path.close();
                        path.build().ok()
                    };
                    if let Some(outline_path) = arrow(-1.0, 1.15) {
                        window.paint_path(outline_path, outline);
                    }
                    if let Some(fill_path) = arrow(0.0, 1.0) {
                        window.paint_path(fill_path, fill);
                    }
                },
            )
            .absolute()
            .size_full(),
        )
    }
}

impl Drop for ScreenView {
    fn drop(&mut self) {
        self.send(ScreenRequest::Close(self.stream));
    }
}

const fn proto_button(button: MouseButton) -> ProtoButton {
    match button {
        MouseButton::Left => ProtoButton::Left,
        MouseButton::Right => ProtoButton::Right,
        MouseButton::Middle => ProtoButton::Middle,
        MouseButton::Navigate(gpui::NavigationDirection::Back) => ProtoButton::Back,
        MouseButton::Navigate(gpui::NavigationDirection::Forward) => ProtoButton::Forward,
    }
}

impl Render for ScreenView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let record_bounds = canvas(
            move |bounds, _window, cx| {
                entity.update(cx, |this, _| this.bounds = bounds);
            },
            |_bounds, (), _window, _cx| {},
        )
        // `inset_0` matters: an absolute element without insets sits at its static position,
        // which for a later sibling is *below* the picture, one body-height off.
        .absolute()
        .inset_0();

        let picture = self.latest.as_ref().map_or_else(
            || {
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.0))
                    .text_color(hsla(self.theme.surfaces.text_muted))
                    .font_family(self.theme.typography.ui_family.clone())
                    .child("waiting for the first frame…")
                    .into_any_element()
            },
            |(_frame, buffer)| {
                surface(buffer.clone()).object_fit(ObjectFit::Fill).size_full().into_any_element()
            },
        );

        div()
            .id("screen")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(hsla(self.theme.surfaces.canvas))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .child(picture)
            .child(record_bounds)
            .children(self.cursor_overlay())
    }
}
