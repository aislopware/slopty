//! What the workspace does for a remote window or desktop beyond its picture, asked from the
//! view's controls ([`crate::screen::Control`]): on a Mac, the system's own shortcuts sent to a
//! remote Mac; the curtain over a remote Mac's screens; a display the worker makes for this
//! device, sized to the view and following it as it changes; and this device's clipboard typed
//! in. On a phone or a tablet a desktop streams from a display made for the device unasked.
//!
//! What it offers each view is said to the view ([`ScreenView::set_offers`]) whenever it moves:
//! a worker's caps, the curtain, the display made for this device, the tap.

use std::collections::{HashMap, HashSet};

use gpui::{AnyWindowHandle, Context, Entity, Focusable as _, Task};
use slopty_client::WorkerKey;
use slopty_client::screen::display;
use slopty_core::{ItemId, StreamId};
use slopty_proto::ClientMsg;
use slopty_proto::items::ItemKind;
use slopty_proto::screen::{
    CurtainState, DisplayKey, DisplayShape, NoVirtualDisplay, Quality, ScreenRequest,
    VirtualDisplay,
};

use super::WorkspaceView;
use crate::screen::{Control, Offers, ScreenView, TYPE_MAX};

/// What a notice says when macOS kept its hotkeys and only the tap's own list goes.
pub const ONLY_CHORDS: &str = "macOS kept its shortcuts: only the app switcher, Spotlight, \
    Spaces and screenshots go to the remote Mac";

/// What a notice says when the tap needs Accessibility first.
pub const ASK_ACCESSIBILITY: &str = "Allow Slopty in Accessibility to send system shortcuts";

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

/// The display a worker streams for this device's key, and the view it shows in: one key has
/// one display on a worker, so a worker has one at most.
#[derive(Debug)]
pub(super) struct Sized {
    /// The display item streamed from it.
    item: ItemId,
    /// The shape the view last rested at: what an open asks for.
    shape: DisplayShape,
    /// An `OpenDisplay` is in flight.
    opening: bool,
    /// The stream the worker opened for it, once its `Display` said.
    stream: Option<StreamId>,
}

impl Sized {
    const fn new(item: ItemId, shape: DisplayShape) -> Self {
        Self { item, shape, opening: false, stream: None }
    }

    /// The display item streamed from it.
    pub(super) const fn item(&self) -> ItemId {
        self.item
    }

    /// Its stream is gone (the link dropped): the next reconcile opens another at its shape.
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
        true
    }

    /// Whether `stream` is (or is being opened as) its own.
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

/// What a worker's curtain stands at, for this device.
#[derive(Debug)]
pub(super) struct Curtain {
    /// What the worker last said of it.
    state: CurtainState,
    /// This device holds it over the Mac: asked again on each link.
    wanted: bool,
    /// An ask is on its way, not yet answered.
    asked: bool,
}

/// The workspace's remote-desktop state.
pub(super) struct Desktop {
    /// This device's display key, read from the data directory once asked for.
    key: Option<DisplayKey>,
    /// What takes the system's shortcuts; `None` where there is nothing to take them from.
    keys: Option<Box<dyn KeyPort>>,
    /// The tap is up, and the task that hands its chords to the view with the keyboard.
    tapping: Option<Task<()>>,
    /// The tap takes chords now.
    armed: bool,
    /// The view whose stream is being replaced while it had the keyboard: the next view of the
    /// item takes it, in this window.
    refocus: Option<(ItemId, AnyWindowHandle)>,
    /// Whether a desktop streams from a display made for this device unless the person picked
    /// otherwise: on a touch device, whose screen is nothing like the Mac's.
    sized_first: bool,
    /// The desktops the person picked a display for: made for this device (`true`) or the
    /// physical one.
    chosen: HashMap<ItemId, bool>,
    /// The workers that could not make this device a display on their current link: not asked
    /// again unasked until they link again.
    declined: HashSet<WorkerKey>,
}

impl Default for Desktop {
    fn default() -> Self {
        #[cfg(target_os = "macos")]
        let keys: Option<Box<dyn KeyPort>> = Some(Box::new(SessionTap::default()));
        #[cfg(not(target_os = "macos"))]
        let keys: Option<Box<dyn KeyPort>> = None;
        Self {
            key: None,
            keys,
            tapping: None,
            armed: false,
            refocus: None,
            sized_first: cfg!(target_os = "ios"),
            chosen: HashMap::new(),
            declined: HashSet::new(),
        }
    }
}

impl Default for Curtain {
    fn default() -> Self {
        Self { state: CurtainState::Down, wanted: false, asked: false }
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
    /// The worker and the kind of the item `id`.
    fn screen_item(&self, id: ItemId) -> Option<(WorkerKey, ItemKind)> {
        self.items().find(|(_, item)| item.id == id).map(|(key, item)| (key, item.kind))
    }

    /// Whether `worker` is a Mac, whose system shortcuts this Mac's mean something to.
    fn worker_is_mac(&self, worker: WorkerKey) -> bool {
        self.workers
            .get(&worker)
            .and_then(|w| w.caps.as_ref())
            .is_some_and(|caps| caps.os == slopty_proto::server::Os::MacOs)
    }

    /// Whether `worker` is linked and can make a display for this device.
    fn offers_displays(&self, worker: WorkerKey) -> bool {
        self.workers.get(&worker).is_some_and(|w| {
            w.is_linked() && w.caps.as_ref().is_some_and(|caps| caps.virtual_displays)
        })
    }

    /// Whether `worker` can draw the curtain over its Mac, or this device holds it there: the
    /// way to lift a curtain held stays while the worker's caps say it draws none.
    fn draws_curtain(&self, worker: WorkerKey) -> bool {
        self.workers
            .get(&worker)
            .is_some_and(|w| w.curtain.wanted || w.caps.as_ref().is_some_and(|caps| caps.curtain))
    }

    /// What the view of `id` is offered, as things stand.
    fn offers_for(&self, id: ItemId, view: &ScreenView) -> Offers {
        let Some((worker, kind)) = self.screen_item(id) else { return Offers::default() };
        let display = matches!(kind, ItemKind::Display { .. });
        let sized = self.workers.get(&worker).and_then(|w| w.sized.as_ref()).map(Sized::item);
        Offers {
            system_keys: (self.desktop.keys.is_some() && self.worker_is_mac(worker))
                .then(|| view.system_keys()),
            curtain: (display && self.draws_curtain(worker))
                .then(|| self.workers.get(&worker).is_some_and(|w| w.curtain.wanted)),
            sized: (display && self.offers_displays(worker)).then(|| sized == Some(id)),
            type_clipboard: true,
        }
    }

    /// Say to every view what it is offered now.
    pub(super) fn refresh_offers(&self, cx: &mut Context<Self>) {
        let views: Vec<(ItemId, Entity<ScreenView>)> =
            self.screens.iter().map(|(id, view)| (*id, view.clone())).collect();
        for (id, view) in views {
            let offers = self.offers_for(id, view.read(cx));
            view.update(cx, |v, cx| v.set_offers(offers, cx));
        }
    }

    /// A row of the view of `id`'s controls asked for `control`.
    pub(super) fn screen_control(&mut self, id: ItemId, control: Control, cx: &mut Context<Self>) {
        match control {
            Control::SystemKeys => self.flip_system_keys(id, cx),
            Control::Curtain => {
                if let Some((worker, _)) = self.screen_item(id) {
                    self.flip_curtain(worker, cx);
                }
            }
            Control::SizedDisplay => self.toggle_sized_display(id, cx),
            Control::TypeClipboard => self.type_clipboard(id, cx),
        }
        self.refresh_offers(cx);
        // The shell's surface draws whichever view the item has now.
        cx.notify();
    }

    // ----- system shortcuts ------------------------------------------------------------------

    /// Send the system's shortcuts to the remote Mac of `id` while its view has the keyboard,
    /// or leave them to this Mac. The first time, the tap is made; without Accessibility the
    /// person is asked for it and the view keeps to this Mac.
    fn flip_system_keys(&mut self, id: ItemId, cx: &mut Context<Self>) {
        let Some(view) = self.screens.get(&id).cloned() else { return };
        let Some((worker, _)) = self.screen_item(id) else { return };
        let on = !view.read(cx).system_keys();
        if on && !self.tap_system_keys(cx) {
            if let Some(keys) = &self.desktop.keys {
                keys.ask();
            }
            Self::show_notice(ASK_ACCESSIBILITY.to_owned(), cx);
            return;
        }
        view.update(cx, |v, cx| v.set_system_keys(on, cx));
        let name = self.workers.get(&worker).map(|w| w.name.clone()).unwrap_or_default();
        let text = if on {
            format!("System shortcuts go to {name}")
        } else {
            "System shortcuts stay on this Mac".to_owned()
        };
        Self::show_notice(text, cx);
        self.arm_system_keys(cx);
    }

    /// The view that has the keyboard in the window in front, when one does.
    fn keyboard_screen(&self, cx: &gpui::App) -> Option<Entity<ScreenView>> {
        self.screens.values().find(|view| view.read(cx).has_keyboard()).cloned()
    }

    /// The tap is up, made now if it was not: its chords go to the view with the keyboard.
    /// Whether it taps.
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
                    let view = this.keyboard_screen(cx).filter(|v| v.read(cx).system_keys());
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

    /// Arm the tap while a view that sends system shortcuts has the keyboard in the window in
    /// front; disarm it the moment that ends ([`crate::screen::ScreenViewEvent::Keyboard`]). The
    /// tap itself lets the shortcuts be as soon as another app is in front. Armed with only the
    /// tap's own list (macOS kept its hotkeys), a notice says which go.
    pub(super) fn arm_system_keys(&mut self, cx: &mut Context<Self>) {
        if self.desktop.tapping.is_none() {
            return;
        }
        let armed = self.keyboard_screen(cx).is_some_and(|view| view.read(cx).system_keys());
        if armed == self.desktop.armed {
            return;
        }
        self.desktop.armed = armed;
        let taking = self.desktop.keys.as_ref().map_or(Taking::Off, |keys| keys.arm(armed));
        if taking == Taking::Chords {
            Self::show_notice(ONLY_CHORDS.to_owned(), cx);
        }
    }

    // ----- the curtain -----------------------------------------------------------------------

    /// Hold the curtain over `worker`'s Mac from this device, or let it go. The hold is this
    /// device's: asked again on each link, so a link that blips keeps it, and the worker lowers
    /// it (and locks the Mac) only once this device stays gone.
    fn flip_curtain(&mut self, worker: WorkerKey, cx: &mut Context<Self>) {
        if !self.draws_curtain(worker) {
            return;
        }
        let Some(w) = self.workers.get_mut(&worker) else { return };
        let on = !w.curtain.wanted;
        if !w.send(ClientMsg::Screen(ScreenRequest::Curtain { on })) {
            let text = format!("{} is out of reach, so the curtain stays as it is", w.name);
            Self::show_notice(text, cx);
            return;
        }
        w.curtain.wanted = on;
        w.curtain.asked = on;
    }

    /// `worker`'s link came up: this device's hold on its curtain is asked again, and what
    /// stands there now comes in the worker's greeting; it may be asked for a display again.
    pub(super) fn desktop_relinked(&mut self, worker: WorkerKey) {
        self.desktop.declined.remove(&worker);
        let Some(w) = self.workers.get_mut(&worker) else { return };
        w.curtain.state = CurtainState::Down;
        w.curtain.asked = false;
        if w.curtain.wanted {
            w.curtain.asked = w.send(ClientMsg::Screen(ScreenRequest::Curtain { on: true }));
        }
    }

    /// `worker` said where its curtain stands. A refusal while this device held it lets the
    /// hold go and says why; a curtain up while the Mac's own input could not be held says
    /// what it needs. Down while this device held it and asked nothing since lets the hold
    /// go: the worker no longer holds it for this device. A Down said before the worker
    /// answered this device's ask is older than the ask and changes nothing.
    pub(super) fn curtain_heard(
        &mut self,
        worker: WorkerKey,
        state: CurtainState,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.workers.get_mut(&worker) else { return };
        let was_up = matches!(w.curtain.state, CurtainState::Up { .. });
        let asked = w.curtain.asked;
        w.curtain.asked = asked && state == CurtainState::Down;
        let said = match &state {
            CurtainState::Refused { why } if w.curtain.wanted => {
                w.curtain.wanted = false;
                Some(format!("No curtain over {}: {why}", w.name))
            }
            CurtainState::Down if w.curtain.wanted && !asked => {
                w.curtain.wanted = false;
                Some(format!("The curtain over {} is down", w.name))
            }
            CurtainState::Up { input_held: false, .. } if w.curtain.wanted && !was_up => {
                Some(format!(
                    "{}'s screens are covered, but its own keyboard and pointer still work: allow Slopty in Accessibility there",
                    w.name
                ))
            }
            _ => None,
        };
        w.curtain.state = state;
        if let Some(said) = said {
            Self::show_notice(said, cx);
        }
        self.refresh_offers(cx);
    }

    // ----- the clipboard typed ---------------------------------------------------------------

    /// This device's clipboard text typed into the view of `id` in paced bursts, the first
    /// [`TYPE_MAX`] bytes of it: for a field that refuses a paste, or a login window.
    fn type_clipboard(&self, id: ItemId, cx: &mut Context<Self>) {
        let Some(view) = self.screens.get(&id).cloned() else { return };
        let text = cx.read_from_clipboard().and_then(|item| item.text()).filter(|t| !t.is_empty());
        let Some(text) = text else {
            Self::show_notice("The clipboard holds no text".to_owned(), cx);
            return;
        };
        if view.update(cx, |v, cx| v.type_text(&text, cx)) {
            let kb = TYPE_MAX / 1024;
            Self::show_notice(format!("Typed the first {kb} KB of the clipboard"), cx);
        }
    }

    // ----- a display made for this device ----------------------------------------------------

    /// This device's display key, drawn in the data directory the first time.
    fn display_key(&mut self) -> Option<DisplayKey> {
        if self.desktop.key.is_none() {
            match display::key(&slopty_platform::dirs::data_dir()) {
                Ok(key) => self.desktop.key = Some(key),
                Err(e) => tracing::warn!(error = %e, "display key not kept"),
            }
        }
        self.desktop.key
    }

    /// This device's display key, once a display has been asked for.
    pub(super) const fn desktop_key(&self) -> Option<DisplayKey> {
        self.desktop.key
    }

    /// Whether the desktop `id` streams from a display made for this device: as the person
    /// picked, else as this device does unasked.
    fn wants_sized(&self, id: ItemId) -> bool {
        self.desktop.chosen.get(&id).copied().unwrap_or(self.desktop.sized_first)
    }

    /// The display made for this device that `id`'s view stands for, at the size it rests at.
    fn shape_of(&self, id: ItemId, cx: &gpui::App) -> Option<DisplayShape> {
        let view = self.screens.get(&id)?.read(cx);
        let (pixels, scale) = view.body_pixels()?;
        Some(display::shape(pixels, scale, crate::screen::main_refresh_hz()))
    }

    /// Close the stream of the view of `id` and let the view go: the item streams some other
    /// way next.
    fn end_screen(&mut self, id: ItemId, cx: &mut gpui::App) {
        if let Some(view) = self.screens.remove(&id) {
            view.update(cx, |v, _| v.end());
        }
    }

    /// Stream the desktop `id` from a display its worker makes for this device, sized to its
    /// view; again, from the physical display. Another desktop of the same worker that had one
    /// goes back to its physical display (one key, one display). The view with the keyboard
    /// keeps it across the swap.
    fn toggle_sized_display(&mut self, id: ItemId, cx: &mut Context<Self>) {
        let Some((worker, ItemKind::Display { .. })) = self.screen_item(id) else { return };
        let window =
            self.screens.get(&id).filter(|v| v.read(cx).has_keyboard()).and(self.keyboard_window);
        self.desktop.refocus = window.map(|window| (id, window));
        let was = self.workers.get(&worker).and_then(|w| w.sized.as_ref()).map(Sized::item);
        if was == Some(id) {
            if let Some(w) = self.workers.get_mut(&worker) {
                w.sized = None;
            }
            self.desktop.chosen.insert(id, false);
            self.end_screen(id, cx);
            self.reconcile_screens();
            return;
        }
        if !self.size_display(worker, id, cx) {
            Self::show_notice("No display could be made for this device".to_owned(), cx);
            return;
        }
        self.desktop.chosen.insert(id, true);
        if let Some(old) = was {
            self.end_screen(old, cx);
        }
        self.reconcile_screens();
    }

    /// Stream the desktop `id` of `worker` from a display made for this device, sized to its
    /// view: its physical display's stream goes. Whether it will.
    fn size_display(&mut self, worker: WorkerKey, id: ItemId, cx: &mut gpui::App) -> bool {
        if !self.offers_displays(worker) || self.display_key().is_none() {
            return false;
        }
        let Some(shape) = self.shape_of(id, cx) else { return false };
        let Some(w) = self.workers.get_mut(&worker) else { return false };
        w.sized = Some(Sized::new(id, shape));
        // The physical display's stream goes.
        self.end_screen(id, cx);
        true
    }

    /// A desktop's view came to rest at a size: on a touch device one that wants a display of
    /// its own takes one now, and a display made for this device takes the new size.
    pub(super) fn screen_rested(&mut self, id: ItemId, cx: &mut Context<Self>) {
        let Some((worker, ItemKind::Display { .. })) = self.screen_item(id) else { return };
        let sized = self.workers.get(&worker).and_then(|w| w.sized.as_ref());
        match sized {
            None if self.wants_sized(id)
                && !self.desktop.declined.contains(&worker)
                && self.offers_displays(worker) =>
            {
                if !self.size_display(worker, id, cx) {
                    self.desktop.declined.insert(worker);
                }
                self.reconcile_screens();
                self.refresh_offers(cx);
                cx.notify();
            }
            Some(sized) if sized.item() == id => {
                let Some(shape) = self.shape_of(id, cx) else { return };
                let Some(w) = self.workers.get_mut(&worker) else { return };
                let Some(sized) = w.sized.as_mut() else { return };
                if sized.shape == shape {
                    return;
                }
                sized.shape = shape;
                if let Some(stream) = sized.stream.filter(|_| !sized.opening) {
                    let pixels = (shape.width, shape.height);
                    w.send(display::resize(stream, pixels, Some(shape.scale)));
                }
            }
            _ => {}
        }
    }

    /// `worker` could not make this device a display: not asked again unasked on this link,
    /// and its desktop goes back to its physical display. The item it was for.
    pub(super) fn display_declined(&mut self, worker: WorkerKey) -> Option<ItemId> {
        self.desktop.declined.insert(worker);
        self.workers.get_mut(&worker)?.sized.take().map(|s| s.item())
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
            Self::show_notice(text, cx);
        }
    }

    /// The view of `id` is new: it takes the keyboard back when the one it replaced had it.
    pub(super) fn screen_arrived(
        &mut self,
        id: ItemId,
        view: &Entity<ScreenView>,
        cx: &mut Context<Self>,
    ) {
        let offers = self.offers_for(id, view.read(cx));
        view.update(cx, |v, cx| v.set_offers(offers, cx));
        let Some((item, window)) = self.desktop.refocus else { return };
        if item != id {
            return;
        }
        self.desktop.refocus = None;
        let focus = view.read(cx).focus_handle(cx);
        let _gone = window.update(cx, |_, window, cx| window.focus(&focus, cx));
    }
}

#[cfg(test)]
mod tests;
