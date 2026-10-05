//! What the palette does to a remote window or desktop tile beyond its header: type this
//! device's clipboard into it, stream a display tile from a display the worker makes for this
//! device (sized to the tile, following it as it resizes), and on the Mac send the system's own
//! shortcuts to a remote Mac.

use std::time::Instant;

use gpui::{Context, Focusable as _, Task, Window};
use slopty_client::layout::{Frame, Rect, WorkerKey};
use slopty_client::screen::display::{self, Follow};
use slopty_core::{ItemId, StreamId};
use slopty_proto::ClientMsg;
use slopty_proto::items::ItemKind;
use slopty_proto::screen::{DisplayKey, DisplayShape, NoVirtualDisplay, Quality, VirtualDisplay};

use super::WorkspaceView;
use super::actions::{ToggleSizedDisplay, ToggleSystemKeys, TypeClipboard};
use crate::palette::PaletteItem;
use crate::screen::TYPE_MAX;

/// The palette's name for a display tile streamed from its physical display.
pub const OPEN_SIZED: &str = "Open a display sized to this window";
/// The palette's name for a display tile streamed from a display made for it.
pub const BACK_TO_PHYSICAL: &str = "Back to the physical display";
/// The palette's name for [`TypeClipboard`].
pub const TYPE_CLIPBOARD: &str = "Type the clipboard";
/// The palette's name for [`ToggleSystemKeys`] while the Mac keeps its shortcuts, and the
/// header's control while they go to the worker.
pub const SEND_SYSTEM_KEYS: &str = "Send system shortcuts";
/// The palette's name for [`ToggleSystemKeys`] while the shortcuts go to the worker.
pub const KEEP_SYSTEM_KEYS: &str = "Keep system shortcuts on this Mac";
/// What a notice says when macOS kept its hotkeys and only the tap's own list goes.
pub const ONLY_CHORDS: &str = "macOS kept its shortcuts: only the app switcher, Spotlight, \
    Spaces and screenshots go to the remote Mac";

/// What takes the system's shortcuts off this Mac: the session tap
/// ([`slopty_platform::system_keys::Tap`]), or a stand-in in tests, which never make a tap.
pub trait KeyPort {
    /// Start tapping, the chords taken going to `chords`; whether it taps.
    fn install(&mut self, chords: tokio::sync::mpsc::UnboundedSender<Chord>) -> bool;
    /// Take chords now, or let them be: which go.
    fn arm(&self, on: bool) -> Taking;
    /// Ask the person for what tapping needs (Accessibility).
    fn ask(&self);
}

/// A chord taken off this Mac.
pub type Chord = slopty_platform::system_keys::Chord;

/// Which of the system's shortcuts the tap takes.
pub type Taking = slopty_platform::system_keys::Taking;

/// The session tap, made the first time the person turns system shortcuts on.
#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
struct SessionTap(Option<slopty_platform::system_keys::Tap>);

#[cfg(target_os = "macos")]
impl KeyPort for SessionTap {
    fn install(&mut self, chords: tokio::sync::mpsc::UnboundedSender<Chord>) -> bool {
        if self.0.is_none() {
            match slopty_platform::system_keys::Tap::install(chords) {
                Ok(tap) => self.0 = Some(tap),
                Err(e) => tracing::info!(error = %e, "system shortcuts not tapped"),
            }
        }
        self.0.is_some()
    }

    fn arm(&self, on: bool) -> Taking {
        self.0.as_ref().map_or(Taking::Off, |tap| tap.arm(on))
    }

    fn ask(&self) {
        let _asked = slopty_platform::system_keys::request();
    }
}

/// The display tile of one worker streamed from a display made for this device. One key has
/// one display on a worker, so a worker has one such tile at most.
#[derive(Debug)]
pub(super) struct Sized {
    /// The display tile.
    item: ItemId,
    /// The shape the tile last drew at: what an open asks for.
    shape: DisplayShape,
    /// An `OpenDisplay` is in flight.
    opening: bool,
    /// The stream the worker opened for it, once its `Display` said.
    stream: Option<StreamId>,
    /// The display following the tile, while its stream is open.
    follow: Follow,
}

impl Sized {
    const fn new(item: ItemId, shape: DisplayShape) -> Self {
        Self { item, shape, opening: false, stream: None, follow: Follow::new(shape) }
    }

    /// The tile streamed from it.
    pub(super) const fn item(&self) -> ItemId {
        self.item
    }

    /// Its stream is gone (the link dropped, or the tile went off screen): the next reconcile
    /// opens another at the tile's shape.
    pub(super) const fn lost(&mut self) {
        self.opening = false;
        self.stream = None;
    }

    /// `stream` is the one opened for it.
    pub(super) fn opened(&mut self, stream: StreamId) -> bool {
        if self.stream != Some(stream) {
            return false;
        }
        self.opening = false;
        self.follow = Follow::new(self.shape);
        true
    }

    /// Whether `stream` is (or is being opened as) this tile's.
    pub(super) fn streams(&self, stream: StreamId) -> bool {
        self.stream == Some(stream)
    }

    /// The `OpenDisplay` to send for it, when it has neither a stream nor one on the way.
    pub(super) const fn open(&mut self, key: DisplayKey, quality: Quality) -> Option<ClientMsg> {
        if self.opening || self.stream.is_some() {
            return None;
        }
        self.opening = true;
        Some(display::open(key, self.shape, quality))
    }
}

/// The workspace's remote-desktop state.
pub(super) struct Desktop {
    /// This device's display key, read from beside the layout once asked for.
    key: Option<DisplayKey>,
    /// Wakes the frame a following display's resize is due at, and when.
    wake: Option<(Instant, Task<()>)>,
    /// What takes the system's shortcuts; `None` where there is nothing to take them from.
    keys: Option<Box<dyn KeyPort>>,
    /// The tap is up, and the task that hands its chords to the tile with the keyboard.
    tapping: Option<Task<()>>,
    /// The tap takes chords now.
    armed: bool,
    /// The workspace's window is the key window with a remote picture holding its keyboard, as
    /// its last frame or its activation found.
    in_main: bool,
    /// Disarms the tap the moment the workspace's window stops being the key window, rather than
    /// at its next frame, which an inactive window may never draw.
    deactivated: Option<gpui::Subscription>,
    /// A tile whose view went while it had the keyboard: its next view takes it.
    refocus: Option<ItemId>,
}

impl Default for Desktop {
    fn default() -> Self {
        #[cfg(target_os = "macos")]
        let keys: Option<Box<dyn KeyPort>> = Some(Box::new(SessionTap::default()));
        #[cfg(not(target_os = "macos"))]
        let keys: Option<Box<dyn KeyPort>> = None;
        Self {
            key: None,
            wake: None,
            keys,
            tapping: None,
            armed: false,
            in_main: false,
            deactivated: None,
            refocus: None,
        }
    }
}

impl std::fmt::Debug for Desktop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Desktop")
            .field("key", &self.key.is_some())
            .field("armed", &self.armed)
            .finish_non_exhaustive()
    }
}

/// The pixels of a tile's body resting at `rect`, under a header `header` points tall, on a
/// screen of backing `scale`.
fn body_pixels(rect: Rect, header: f32, scale: f32) -> (u32, u32) {
    let px = |points: f32| {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "≥ 1, small")]
        let out = (points * scale).round().max(1.0) as u32;
        out
    };
    (px(rect.w), px(rect.h - header))
}

/// Why a worker streamed a physical display where this device asked for its own, as a notice
/// says it.
const fn why_physical(why: NoVirtualDisplay) -> &'static str {
    match why {
        NoVirtualDisplay::Unavailable => "it cannot make one",
        NoVirtualDisplay::Refused => "macOS refused it",
        NoVirtualDisplay::Unsettled => "it never settled",
        NoVirtualDisplay::Unlisted => "screen capture never saw it",
    }
}

impl WorkspaceView {
    /// Whether `worker` is linked and can make a display for this device, and this device has
    /// somewhere to keep its key.
    fn offers_displays(&self, worker: WorkerKey) -> bool {
        self.layout_path.is_some()
            && self.workers.get(&worker).is_some_and(|w| {
                w.is_linked() && w.caps.as_ref().is_some_and(|caps| caps.virtual_displays)
            })
    }

    /// This device's display key, once a display has been asked for.
    pub(super) const fn desktop_key(&self) -> Option<DisplayKey> {
        self.desktop.key
    }

    /// This device's display key, drawn beside the layout the first time.
    fn display_key(&mut self) -> Option<DisplayKey> {
        if self.desktop.key.is_none() {
            let dir = self.layout_path.as_ref()?.parent()?;
            match display::key(dir) {
                Ok(key) => self.desktop.key = Some(key),
                Err(e) => tracing::warn!(error = %e, "display key not kept"),
            }
        }
        self.desktop.key
    }

    /// The palette's lines for the focused remote tile: type the clipboard into it, and on a
    /// display whose worker can make one, a display sized to it (or back to the physical one).
    pub(super) fn screen_lines(&self, cx: &gpui::App) -> Vec<PaletteItem> {
        let Some(tile) = self.focused() else { return Vec::new() };
        let Some(item) = self.item(tile) else { return Vec::new() };
        let bindings = super::actions::key_bindings();
        let line =
            |label: &str, action: Box<dyn gpui::Action>| PaletteItem::new(label, action, &bindings);
        let mut lines = Vec::new();
        if matches!(item.kind, ItemKind::Window { .. } | ItemKind::Display { .. })
            && self.screens.contains_key(&item.id)
        {
            lines.push(line(TYPE_CLIPBOARD, Box::new(TypeClipboard)));
        }
        lines.extend(self.own_window_line(item, &bindings));
        if let Some(view) = self.screens.get(&item.id)
            && self.desktop.keys.is_some()
            && self.worker_is_mac(tile.worker)
        {
            let on = view.read(cx).system_keys();
            let label = if on { KEEP_SYSTEM_KEYS } else { SEND_SYSTEM_KEYS };
            lines.push(line(label, Box::new(ToggleSystemKeys)));
        }
        if matches!(item.kind, ItemKind::Display { .. }) && self.offers_displays(tile.worker) {
            let sized = self
                .workers
                .get(&tile.worker)
                .and_then(|w| w.sized.as_ref())
                .is_some_and(|s| s.item == item.id);
            let label = if sized { BACK_TO_PHYSICAL } else { OPEN_SIZED };
            lines.push(line(label, Box::new(ToggleSizedDisplay)));
        }
        lines
    }

    /// Whether `worker` is a Mac, whose system shortcuts this Mac's mean something to.
    fn worker_is_mac(&self, worker: WorkerKey) -> bool {
        self.workers
            .get(&worker)
            .and_then(|w| w.caps.as_ref())
            .is_some_and(|caps| caps.os == slopty_proto::server::Os::MacOs)
    }

    /// Use `keys` to take the system's shortcuts, in place of the session tap.
    #[cfg(test)]
    pub(crate) fn set_key_port(&mut self, keys: Box<dyn KeyPort>) {
        self.desktop.keys = Some(keys);
    }

    /// Send the system's shortcuts to the focused remote Mac while its tile has the keyboard,
    /// or leave them to this Mac. The first time, the tap is made; without Accessibility the
    /// person is asked for it and the tile keeps to this Mac.
    pub fn toggle_system_keys(
        &mut self,
        _: &ToggleSystemKeys,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tile) = self.focused() {
            self.flip_system_keys(tile.item, cx);
        }
    }

    /// Flip system shortcuts on the remote tile `item` (the palette, or its header's control).
    pub(super) fn flip_system_keys(&mut self, item: ItemId, cx: &mut Context<Self>) {
        let Some(view) = self.screens.get(&item).cloned() else { return };
        let Some(tile) = self.tile_of(item) else { return };
        let on = !view.read(cx).system_keys();
        if on && !self.tap_system_keys(cx) {
            if let Some(keys) = &self.desktop.keys {
                keys.ask();
            }
            let text = "Allow Slopty in Accessibility to send system shortcuts".to_owned();
            self.show_notice(text, cx);
            return;
        }
        view.update(cx, |v, cx| v.set_system_keys(on, cx));
        let name = self.workers.get(&tile.worker).map(|w| w.name.clone()).unwrap_or_default();
        let text = if on {
            format!("System shortcuts go to {name}")
        } else {
            "System shortcuts stay on this Mac".to_owned()
        };
        self.show_notice(text, cx);
        cx.notify();
    }

    /// The tap is up, made now if it was not: its chords go to the remote tile with the
    /// keyboard. Whether it taps.
    fn tap_system_keys(&mut self, cx: &Context<Self>) -> bool {
        if self.desktop.tapping.is_some() {
            return true;
        }
        let Some(keys) = self.desktop.keys.as_mut() else { return false };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        if !keys.install(tx) {
            return false;
        }
        self.desktop.tapping = Some(cx.spawn(async move |this, cx| {
            while let Some(chord) = rx.recv().await {
                let sent = this.update(cx, |this, cx| {
                    let view = this.active_screen().filter(|v| v.read(cx).system_keys());
                    if let Some(view) = view {
                        view.update(cx, |v, cx| {
                            v.system_key(chord.code, chord.down, chord.mods, cx);
                        });
                    }
                });
                if sent.is_err() {
                    break;
                }
            }
        }));
        true
    }

    /// Arm the tap while a remote tile that sends system shortcuts has the keyboard in the
    /// active window, and nothing is over it; disarm it the moment one of those ends. The
    /// window going inactive disarms it as it happens; the tap itself lets the shortcuts be as
    /// soon as another app is in front. Armed with only the tap's own list (macOS kept its
    /// hotkeys), a notice says which go.
    pub(super) fn arm_system_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.desktop.tapping.is_none() {
            return;
        }
        if self.desktop.deactivated.is_none() {
            let observed = cx.observe_window_activation(window, |this, window, cx| {
                this.arm_system_keys(window, cx);
            });
            self.desktop.deactivated = Some(observed);
        }
        self.desktop.in_main = window.is_window_active()
            && self
                .active_screen()
                .is_some_and(|view| view.read(cx).focus_handle(cx).is_focused(window));
        self.rearm_system_keys(cx);
    }

    /// Arm the tap for whichever window has the remote picture's keyboard: the workspace's, as
    /// [`Self::arm_system_keys`] last found it, or the tile's own window while that is the key
    /// window ([`super::popout`]), which holds nothing but the picture. Called as either
    /// window's activation changes, since an inactive window may draw no frame to call it from.
    pub(super) fn rearm_system_keys(&mut self, cx: &mut Context<Self>) {
        if self.desktop.tapping.is_none() {
            return;
        }
        let wants = self.active_screen().is_some_and(|view| view.read(cx).system_keys());
        let in_own = self
            .popouts
            .active()
            .is_some_and(|item| self.focused().is_some_and(|tile| tile.item == item));
        let armed = wants && self.palette.is_none() && (self.desktop.in_main || in_own);
        if armed == self.desktop.armed {
            return;
        }
        self.desktop.armed = armed;
        let taking = self.desktop.keys.as_ref().map_or(Taking::Off, |keys| keys.arm(armed));
        if taking == Taking::Chords {
            self.show_notice(ONLY_CHORDS.to_owned(), cx);
        }
    }

    /// "Type the clipboard": this device's clipboard text typed into the focused remote tile in
    /// paced bursts, the first [`TYPE_MAX`] bytes of it. Named keys (Return, Tab) go as keys and
    /// the rest as text, so any script arrives as written.
    pub fn type_clipboard(
        &mut self,
        _: &TypeClipboard,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.active_screen() else { return };
        let text = cx.read_from_clipboard().and_then(|item| item.text()).filter(|t| !t.is_empty());
        let Some(text) = text else {
            self.show_notice("The clipboard holds no text".to_owned(), cx);
            return;
        };
        if view.update(cx, |v, cx| v.type_text(&text, cx)) {
            let kb = TYPE_MAX / 1024;
            self.show_notice(format!("Typed the first {kb} KB of the clipboard"), cx);
        }
    }

    /// Stream the focused display tile from a display its worker makes for this device, sized
    /// to the tile; run again, from the physical display. Another tile of the same worker that
    /// had one goes back to its physical display (one key, one display).
    pub fn toggle_sized_display(
        &mut self,
        _: &ToggleSizedDisplay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tile) = self.focused() else { return };
        if !matches!(self.item(tile).map(|i| &i.kind), Some(ItemKind::Display { .. })) {
            return;
        }
        // The stream is replaced under the keyboard: the next view of the tile takes it back.
        let had_keys = self
            .screens
            .get(&tile.item)
            .is_some_and(|v| v.read(cx).focus_handle(cx).contains_focused(window, cx));
        self.desktop.refocus = had_keys.then_some(tile.item);
        let was = self.workers.get(&tile.worker).and_then(|w| w.sized.as_ref()).map(Sized::item);
        if was == Some(tile.item) {
            if let Some(w) = self.workers.get_mut(&tile.worker) {
                w.sized = None;
            }
            self.screens.remove(&tile.item);
            self.reconcile_screens();
            cx.notify();
            return;
        }
        if !self.offers_displays(tile.worker) || self.display_key().is_none() {
            return;
        }
        let frame = self.layout.frame();
        let Some(placed) = frame.tiles.iter().find(|p| p.tile == tile) else { return };
        let scale = window.scale_factor();
        let pixels = body_pixels(placed.target, self.header_h(), scale);
        let shape = display::shape(pixels, scale, crate::screen::main_refresh_hz());
        if let Some(w) = self.workers.get_mut(&tile.worker) {
            w.sized = Some(Sized::new(tile.item, shape));
        }
        // Dropping a view closes its stream: the physical display's, and the one a tile of
        // this worker had before.
        self.screens.remove(&tile.item);
        if let Some(old) = was {
            self.screens.remove(&old);
        }
        self.reconcile_screens();
        cx.notify();
    }

    /// The worker says what the stream it opened for this device's key shows: a display made
    /// for it, or a physical one and why.
    pub(super) fn display_told(
        &mut self,
        worker: WorkerKey,
        stream: StreamId,
        key: DisplayKey,
        shown: VirtualDisplay,
        cx: &mut Context<Self>,
    ) {
        if self.desktop.key != Some(key) {
            return;
        }
        let Some(w) = self.workers.get_mut(&worker) else { return };
        let Some(sized) = w.sized.as_mut() else { return };
        if sized.opening && sized.stream.is_none() {
            sized.stream = Some(stream);
        }
        if let VirtualDisplay::Physical { display, why } = shown {
            let text = format!(
                "{} made no display for this device ({}); showing display {}",
                w.name,
                why_physical(why),
                display.0
            );
            self.show_notice(text, cx);
        }
    }

    /// Each display made for this device takes its tile's size once the tile has held it for
    /// [`display::SETTLE`]; the frame then is woken for it. The shape a tile rests at is kept
    /// for the next open.
    pub(super) fn follow_sized_displays(
        &mut self,
        frame: &Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(item) = self.desktop.refocus
            && let Some(view) = self.screens.get(&item)
        {
            self.desktop.refocus = None;
            window.focus(&view.read(cx).focus_handle(cx), cx);
        }
        if self.workers.values().all(|w| w.sized.is_none()) {
            return;
        }
        let now = cx.background_executor().now();
        let scale = window.scale_factor();
        let header = self.header_h();
        let mut due: Option<Instant> = None;
        for w in self.workers.values_mut() {
            let Some(sized) = w.sized.as_mut() else { continue };
            // A tile in a window of its own keeps the display it had.
            if self.popouts.holds(sized.item) {
                continue;
            }
            let Some(placed) = frame.tiles.iter().find(|p| p.tile.item == sized.item) else {
                continue;
            };
            let pixels = body_pixels(placed.target, header, scale);
            sized.shape = display::shape(pixels, scale, sized.shape.refresh_hz);
            let Some(stream) = sized.stream.filter(|_| !sized.opening) else { continue };
            sized.follow.tile(pixels, scale, now);
            let resize = sized.follow.take(stream, now);
            due = match (due, sized.follow.due()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            if let Some(resize) = resize {
                w.send(resize);
            }
        }
        let Some(at) = due else {
            self.desktop.wake = None;
            return;
        };
        if self.desktop.wake.as_ref().is_some_and(|(when, _)| *when == at) {
            return;
        }
        let wait = at.saturating_duration_since(now);
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _gone = this.update(cx, |_, cx| cx.notify());
        });
        self.desktop.wake = Some((at, task));
    }
}
