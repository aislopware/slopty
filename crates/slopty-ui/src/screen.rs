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
    Autocapitalize, Bounds, Context, ElementInputHandler, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent,
    Keystroke, LongPressEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, ParentElement as _, PathBuilder, Pixels, Point, Render, ScrollDelta,
    ScrollWheelEvent, Styled as _, Task, TextInputAction, TextInputConfiguration, TouchPhase,
    UTF16Selection, Window, canvas, div, point, px, surface,
};
use slopty_client::{CursorState, ScreenHandle};
use slopty_codec::DecodedFrame;
use slopty_core::StreamId;
use slopty_proto::ClientMsg;
use slopty_proto::input::{KeyAction, KeyCode, Mods, MouseButton as ProtoButton};
use slopty_proto::screen::{
    CaptureTarget, MAX_CLIPBOARD_BYTES, Quality, ScreenInput, ScreenRequest, ScrollPhase,
    VideoCodec,
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
    /// Pressed: the canvas should make this item active. (The view stops the mouse event so
    /// the canvas does not pan, which also keeps it from the item's own activate handler.)
    Pressed,
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
    /// The text last known to be on the host's pasteboard (received from it, or pushed by
    /// this view ahead of a paste); a paste chord only pushes when the clipboard differs.
    host_clipboard: Option<String>,
    /// Modifiers armed by the phone key bar; applied to the next key, then cleared.
    sticky: Modifiers,
    /// Input-method composition in progress (nothing is sent until it commits).
    marked: Option<String>,
    _pump: Task<()>,
}

/// A modifier the phone key bar can arm for the next key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sticky {
    /// ⌃.
    Control,
    /// ⌘.
    Command,
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
            host_clipboard: None,
            sticky: Modifiers::default(),
            marked: None,
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

    /// Whether the host has sent any audio for this stream (the mute control is pointless
    /// before that).
    #[must_use]
    pub fn has_audio(&self) -> bool {
        self.handle.stats().audio_packets > 0
    }

    /// Whether audio playback is silenced on this client.
    #[must_use]
    pub fn muted(&self) -> bool {
        self.handle.muted()
    }

    /// Silence or resume this stream's audio on this client only.
    /// The pill lives in the canvas title bar, so the caller notifies its own entity.
    pub fn toggle_mute(&self) {
        self.handle.set_muted(!self.handle.muted());
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
        cx.emit(ScreenViewEvent::Pressed);
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
        if is_paste_chord(&ev.keystroke) {
            self.push_clipboard(cx);
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

    /// Before a paste reaches the host, make sure it pastes what this client copied: send the
    /// clipboard text on the (ordered) control stream ahead of the key. Skipped when the host
    /// already has it, or when the clipboard is not text small enough to sync.
    fn push_clipboard(&mut self, cx: &Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        if text.len() > MAX_CLIPBOARD_BYTES || self.host_clipboard.as_deref() == Some(&*text) {
            return;
        }
        self.send(ScreenRequest::Clipboard { text: text.clone() });
        self.host_clipboard = Some(text);
    }

    /// The host's pasteboard changed (the canvas already copied it locally).
    pub fn host_clipboard_changed(&mut self, text: &str) {
        self.host_clipboard = Some(text.to_owned());
    }

    /// Whether `which` is armed for the next key.
    #[must_use]
    pub const fn sticky(&self, which: Sticky) -> bool {
        match which {
            Sticky::Control => self.sticky.control,
            Sticky::Command => self.sticky.platform,
        }
    }

    /// Arm or disarm a modifier for the next key (the phone key bar's ⌃ and ⌘).
    pub fn set_sticky(&mut self, which: Sticky, on: bool, cx: &mut Context<Self>) {
        match which {
            Sticky::Control => self.sticky.control = on,
            Sticky::Command => self.sticky.platform = on,
        }
        cx.notify();
    }

    /// Press and release one key on the host: the phone key bar and the soft keyboard, which
    /// have no key-up of their own. Armed modifiers apply and clear.
    pub fn press(&mut self, mut keystroke: Keystroke, cx: &mut Context<Self>) {
        let armed = std::mem::take(&mut self.sticky);
        keystroke.modifiers.control |= armed.control;
        keystroke.modifiers.platform |= armed.platform;
        if is_paste_chord(&keystroke) {
            self.push_clipboard(cx);
        }
        let code = keys::key_code(&keystroke.key);
        let mods = keys::mods(keystroke.modifiers);
        // A modified key is a chord, not typing: no text, or the host would insert it too.
        let text = keystroke
            .key_char
            .filter(|t| !t.is_empty() && !mods.intersects(Mods::CTRL | Mods::SUPER));
        self.input(ScreenInput::Key { code, action: KeyAction::Press, mods, text });
        self.input(ScreenInput::Key { code, action: KeyAction::Release, mods, text: None });
        cx.notify();
    }

    /// Touch: a long press over the picture is a right click on the host (context menus);
    /// a plain drag stays a canvas pan. Returns whether the gesture was claimed.
    fn long_press(&self, ev: &LongPressEvent, cx: &mut Context<Self>) -> bool {
        if ev.phase != TouchPhase::Started || !self.inside(ev.start_position) {
            return false;
        }
        let (x, y) = self.to_stream(ev.start_position);
        for down in [true, false] {
            self.input(ScreenInput::Button {
                button: ProtoButton::Right,
                down,
                x,
                y,
                clicks: 1,
                mods: keys::mods(Modifiers::default()),
            });
        }
        cx.notify();
        true
    }

    /// The phone's "paste" key: ⌘V on the host, this client's clipboard pushed first.
    pub fn paste_key(&mut self, cx: &mut Context<Self>) {
        self.press(chord("v"), cx);
    }

    /// The phone's "copy" key: ⌘C on the host; the host's pasteboard then flows back here.
    pub fn copy_key(&mut self, cx: &mut Context<Self>) {
        self.press(chord("c"), cx);
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
        let handler = cx.entity();
        let focus = self.focus.clone();
        let record_bounds = canvas(
            move |bounds, _window, cx| {
                entity.update(cx, |this, _| this.bounds = bounds);
            },
            // Registering as a text input is what raises the soft keyboard on iOS and lets an
            // input method compose; typed text arrives in `replace_text_in_range`.
            move |bounds, (), window, cx| {
                window.handle_input(&focus, ElementInputHandler::new(bounds, handler.clone()), cx);
                window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
                    if phase != gpui::DispatchPhase::Bubble {
                        return;
                    }
                    if handler.update(cx, |view, cx| view.long_press(event, cx)) {
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                });
            },
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

/// ⌘ + `key`.
fn chord(key: &str) -> Keystroke {
    Keystroke {
        modifiers: Modifiers { platform: true, ..Modifiers::default() },
        key: key.to_owned(),
        key_char: None,
    }
}

impl EntityInputHandler for ScreenView {
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
        self.marked.as_ref().map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            cx.notify();
        }
    }

    /// Committed text: one key per character so the host sees ordinary typing (a single
    /// event carrying a whole string trips apps that read the key code, not the string).
    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        for c in text.chars() {
            let key = match c {
                '\n' | '\r' => "enter".to_owned(),
                '\t' => "tab".to_owned(),
                ' ' => "space".to_owned(),
                _ => c.to_string(),
            };
            let key_char = (!c.is_control()).then(|| c.to_string());
            self.press(Keystroke { modifiers: Modifiers::default(), key, key_char }, cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!new_text.is_empty()).then(|| new_text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The host's pointer stands in for a caret: candidate windows hang there.
        let (w, h) = (f32::from(self.bounds.size.width), f32::from(self.bounds.size.height));
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let (sw, sh) = ((self.size.0 as f32).max(1.0), (self.size.1 as f32).max(1.0));
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let (x, y) = (self.cursor.x as f32 / sw * w, self.cursor.y as f32 / sh * h);
        let origin = point(self.bounds.origin.x + px(x), self.bounds.origin.y + px(y));
        Some(Bounds::new(origin, gpui::size(px(1.0), px(16.0))))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
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

/// ⌘V (and ⇧⌘V, "paste and match style"): the chords that make the host read its pasteboard.
fn is_paste_chord(keystroke: &Keystroke) -> bool {
    let m = keystroke.modifiers;
    m.platform && !m.control && !m.alt && keystroke.key == "v"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(s: &str) -> Keystroke {
        Keystroke::parse(s).expect("keystroke")
    }

    #[test]
    fn paste_chords() {
        assert!(is_paste_chord(&chord("cmd-v")));
        assert!(is_paste_chord(&chord("cmd-shift-v")));
        assert!(!is_paste_chord(&chord("v")));
        assert!(!is_paste_chord(&chord("ctrl-v")));
        assert!(!is_paste_chord(&chord("cmd-alt-v")));
        assert!(!is_paste_chord(&chord("cmd-c")));
    }
}
