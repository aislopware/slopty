//! A remote window's or desktop's own controls: a quiet "…" in the top right corner of the
//! picture, and the menu it opens.
//!
//! The picture takes every press, key and right click for the worker, so what the person asks of
//! the stream itself cannot be a context menu on it: it is this one button, which floats over the
//! picture as the driver's pill does and keeps its presses from the worker. Over a pointer it
//! shows while the pointer is over the picture or something is wrong with the stream; on a touch
//! screen, which has no hover, it always shows. Beside it, while the stream is unwell, its
//! health mark says how, and opens the stream's stats.
//!
//! The menu holds what the view does itself (the stats, the sound, the phone's trackpad and its
//! gestures, the phone's copy and paste) and what only the workspace can do for it
//! ([`Offers`], asked for with [`ScreenViewEvent::Control`]): the system's shortcuts sent to a
//! remote Mac, the curtain over it, a display made for this device, and the clipboard typed in.

use std::rc::Rc;

use gpui::{
    Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, anchored, deferred, div, point, px,
};
use slopty_kit::colors::hsla;
use slopty_kit::kit::menu::Mark;
use slopty_kit::kit::{self, Menu, MenuItem, MenuPanel};

use super::{ScreenView, ScreenViewEvent};

/// The button's name, and the menu's.
pub const CONTROLS: &str = "Window controls";
/// The stats overlay's row.
pub const STREAM_STATS: &str = "Stream stats";
/// The sound's row, ticked while muted.
pub const MUTE: &str = "Mute";
/// The trackpad's row, on a touch screen.
pub const TRACKPAD: &str = super::TRACKPAD_MODE;
/// The gestures' row, on a touch screen.
pub const GESTURES: &str = super::REMOTE_GESTURES;
/// The phone's paste: ⌘V on the worker, this device's clipboard first.
pub const PASTE: &str = "Paste";
/// The phone's copy: ⌘C on the worker, whose clipboard then comes back here.
pub const COPY: &str = "Copy";
/// The system shortcuts' row, ticked while they go to the worker.
pub const SYSTEM_KEYS: &str = "Send system shortcuts";
/// The curtain's row, ticked while this device holds it over the remote Mac.
pub const CURTAIN: &str = "Curtain over the remote Mac";
/// The sized display's row, ticked while the stream is of a display made for this device.
pub const SIZED_DISPLAY: &str = "Display sized to this window";
/// The row that types this device's clipboard into the window.
pub const TYPE_CLIPBOARD: &str = "Type the clipboard";

/// The group the view's root names, so the button shows while the pointer is over the picture.
pub(super) const GROUP: &str = "screen-view";

/// How far below the body's top edge what hangs under the controls' pill starts: the menu, and
/// the stats, which would cover the button if they shared its corner.
pub(super) fn under_controls(theme: &slopty_theme::Theme) -> f32 {
    let pill = theme.spacing.xs.mul_add(2.0, kit::icon_button_side(theme));
    theme.spacing.sm + pill + theme.spacing.xs
}

/// What only the workspace can do for a view: it says which it offers, and how each stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Offers {
    /// The system's shortcuts can go to the worker (a remote Mac, from a Mac), and whether they
    /// do; `None` where they cannot.
    pub system_keys: Option<bool>,
    /// The worker draws a curtain over its Mac, and whether this device holds it; `None` where
    /// it draws none.
    pub curtain: Option<bool>,
    /// The worker makes displays for a device, and whether this stream is of one made for this
    /// device; `None` for a window, or a worker that makes none.
    pub sized: Option<bool>,
    /// The clipboard can be typed in: the stream is open.
    pub type_clipboard: bool,
}

/// What a row asks of the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// Send the system's shortcuts to the worker, or keep them here.
    SystemKeys,
    /// Draw the curtain over the remote Mac, or lift it.
    Curtain,
    /// Stream a display made for this device, or the physical one.
    SizedDisplay,
    /// Type this device's clipboard into the window.
    TypeClipboard,
}

/// A row of the menu, by what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Stats,
    Mute,
    Trackpad,
    Gestures,
    Paste,
    Copy,
    Ask(Control),
}

impl ScreenView {
    /// What the workspace offers this view, as it stands now.
    pub fn set_offers(&mut self, offers: Offers, cx: &mut Context<Self>) {
        if self.offers != offers {
            self.offers = offers;
            cx.notify();
        }
    }

    /// What the workspace offers this view, as last said.
    #[must_use]
    pub const fn offers(&self) -> Offers {
        self.offers
    }

    /// Whether the controls' menu is open.
    #[must_use]
    pub const fn controls_open(&self) -> bool {
        self.controls_open
    }

    /// Open the controls' menu, or close it and give the picture the keyboard back.
    pub fn set_controls_open(
        &mut self,
        open: bool,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.controls_open == open {
            return;
        }
        self.controls_open = open;
        if !open {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// The menu's rows as they stand, in order, each with its words and its tick.
    fn control_rows(&self) -> Vec<(Row, &'static str, Mark)> {
        let mut rows = vec![(Row::Stats, STREAM_STATS, Mark::Check(self.hud()))];
        if self.has_audio() {
            rows.push((Row::Mute, MUTE, Mark::Check(self.muted())));
        }
        if self.touch {
            rows.push((Row::Trackpad, TRACKPAD, Mark::Check(self.trackpad())));
            rows.push((Row::Gestures, GESTURES, Mark::Check(self.remote_gestures())));
            rows.push((Row::Paste, PASTE, Mark::Plain));
            rows.push((Row::Copy, COPY, Mark::Plain));
        }
        let offers = self.offers;
        if let Some(on) = offers.system_keys {
            rows.push((Row::Ask(Control::SystemKeys), SYSTEM_KEYS, Mark::Check(on)));
        }
        if let Some(held) = offers.curtain {
            rows.push((Row::Ask(Control::Curtain), CURTAIN, Mark::Check(held)));
        }
        if let Some(sized) = offers.sized {
            rows.push((Row::Ask(Control::SizedDisplay), SIZED_DISPLAY, Mark::Check(sized)));
        }
        if offers.type_clipboard {
            rows.push((Row::Ask(Control::TypeClipboard), TYPE_CLIPBOARD, Mark::Plain));
        }
        rows
    }

    /// Do what `row` says, the menu already closed.
    fn control_pick(&mut self, row: Row, cx: &mut Context<Self>) {
        match row {
            Row::Stats => self.set_hud(!self.hud(), cx),
            Row::Mute => {
                self.toggle_mute();
                cx.notify();
            }
            Row::Trackpad => self.toggle_trackpad(cx),
            Row::Gestures => self.toggle_remote_gestures(cx),
            Row::Paste => self.paste_key(cx),
            Row::Copy => self.copy_key(cx),
            Row::Ask(control) => cx.emit(ScreenViewEvent::Control(control)),
        }
    }

    /// The button in the picture's top right corner, the stream's health mark beside it while
    /// it is unwell, and the menu while it is open.
    pub(super) fn controls(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let view = cx.entity();
        let health = Self::health_mark(&view, self.header(), theme, 1.0);
        // Over a pointer the button waits for the pointer, unless the stream is unwell or the
        // menu is open; a finger has no hover, so on touch it stays.
        let always = self.touch || self.controls_open || health.is_some();
        let button = kit::icon_button(
            theme,
            "screen-controls",
            slopty_kit::icons::Symbol::Ellipsis,
            CONTROLS,
        )
        .debug_selector(|| "screen-controls".to_owned())
        .on_click(cx.listener(|this, _ev, window, cx| {
            // A press that found the menu open closed it on its way down (outside the menu):
            // its click leaves it closed rather than opening it again.
            let open = !this.controls_open && !std::mem::take(&mut this.controls_pressed_open);
            this.set_controls_open(open, window, cx);
        }));
        let pill = kit::elevate(kit::pill_frame(theme), theme)
            .id("screen-controls-pill")
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .p(px(theme.spacing.xs))
            .text_color(hsla(theme.surfaces.text_secondary))
            // Before the menu's press outside closes it: the pill paints first, so its capture
            // runs first.
            .capture_any_mouse_down(cx.listener(|this, _ev, _window, _cx| {
                this.controls_pressed_open = this.controls_open;
            }))
            // A press on the pill is the pill's: it never reaches the picture under it.
            .on_mouse_down(gpui::MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .on_mouse_down(gpui::MouseButton::Right, |_ev, _w, cx| cx.stop_propagation())
            .children(health)
            .child(button);
        let pill =
            if always { pill } else { pill.opacity(0.0).group_hover(GROUP, |el| el.opacity(1.0)) };
        div()
            .absolute()
            .top(px(theme.spacing.sm))
            .right(px(theme.spacing.sm))
            .child(pill)
            .children(self.controls_open.then(|| self.controls_menu(cx)))
            .into_any_element()
    }

    /// The open menu, hung under the button's corner and kept inside the window.
    fn controls_menu(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let this = cx.entity().downgrade();
        let mut menu = Menu::new();
        for (row, label, mark) in self.control_rows() {
            let this = this.clone();
            let key = label.to_lowercase().replace(' ', "-");
            menu.push(
                MenuItem::new(key, label, move |_window, cx| {
                    let _gone = this.update(cx, |v, cx| v.control_pick(row, cx));
                })
                .mark(mark),
            );
        }
        let close = this;
        let panel = MenuPanel::new("screen-controls-menu", CONTROLS, Rc::new(menu), &self.theme, {
            move |window, cx| {
                let _gone = close.update(cx, |v, cx| v.set_controls_open(false, window, cx));
            }
        });
        let margin = self.theme.spacing.sm;
        let corner = point(
            self.bounds.origin.x + self.bounds.size.width - px(margin),
            self.bounds.origin.y + px(under_controls(&self.theme)),
        );
        deferred(
            anchored()
                .anchor(gpui::Anchor::TopRight)
                .position(corner)
                .snap_to_window_with_margin(px(margin))
                .child(panel),
        )
        .with_priority(slopty_kit::layer::Layer::Submenu.priority())
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row's words name it once: no two rows say the same.
    #[test]
    fn every_row_has_its_own_words() {
        let words = [
            STREAM_STATS,
            MUTE,
            TRACKPAD,
            GESTURES,
            PASTE,
            COPY,
            SYSTEM_KEYS,
            CURTAIN,
            SIZED_DISPLAY,
            TYPE_CLIPBOARD,
        ];
        let mut seen = std::collections::HashSet::new();
        assert!(words.iter().all(|w| seen.insert(*w)), "{words:?}");
    }
}
