//! slopty: the remote window and desktop surfaces of the right pane ([`RightSurface::Screen`]),
//! beside the browser: the picker's row, the chip, the body and the close, each a call from the
//! shell's own match on the surface.

use std::collections::HashMap;

use gpui::{AnyElement, App, AppContext as _, Context, Entity, Global, IntoElement, SharedString};

use super::{RightSurface, Shell};
use crate::live::screen::ScreenSurface;

/// The screen surfaces, by their tab's number.
#[derive(Default)]
struct Screens {
    seq: u64,
    tabs: HashMap<u64, Entity<ScreenSurface>>,
}

impl Global for Screens {}

impl Shell {
    /// The picker's "Remote window" row: a new surface listing the chat's machine's desktops
    /// and windows.
    pub(super) fn add_screen_surface(&mut self, cx: &mut Context<Self>) {
        let state = self.state.clone();
        let surface = cx.new(|cx| ScreenSurface::new(state, cx));
        let screens = cx.default_global::<Screens>();
        screens.seq += 1;
        let id = screens.seq;
        screens.tabs.insert(id, surface);
        let key = self.panel_key(cx);
        self.right_tabs
            .entry(key)
            .or_default()
            .push(RightSurface::Screen(id));
        self.set_right_active(RightSurface::Screen(id), cx);
    }

    /// The surface showing again `screen`, titled `title`, which the pane showed when the app
    /// last ran.
    pub(super) fn reopen_screen_surface(
        &mut self,
        screen: String,
        title: String,
        cx: &mut Context<Self>,
    ) -> RightSurface {
        let state = self.state.clone();
        let surface = cx.new(|cx| ScreenSurface::reopen(state, screen, title, cx));
        let screens = cx.default_global::<Screens>();
        screens.seq += 1;
        let id = screens.seq;
        screens.tabs.insert(id, surface);
        let key = self.panel_key(cx);
        self.right_tabs.entry(key).or_default().push(RightSurface::Screen(id));
        RightSurface::Screen(id)
    }

    /// What the surface was chosen to show, its id and title, to show it again.
    pub(super) fn screen_surface_chosen(&self, id: u64, cx: &App) -> Option<(String, String)> {
        cx.try_global::<Screens>()?.tabs.get(&id)?.read(cx).chosen()
    }

    /// The surface's chip: its window's title.
    pub(super) fn screen_surface_row(
        &self,
        id: u64,
        cx: &App,
    ) -> Option<(RightSurface, SharedString, bool, Option<SharedString>)> {
        let surface = cx.try_global::<Screens>()?.tabs.get(&id)?;
        Some((RightSurface::Screen(id), surface.read(cx).title(), false, None))
    }

    /// The surface's body, or the picker once it is gone.
    pub(super) fn render_screen_surface(&mut self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        match cx.try_global::<Screens>().and_then(|s| s.tabs.get(&id)).cloned() {
            Some(surface) => surface.into_any_element(),
            None => self.render_surface_picker(cx),
        }
    }

    /// The surface became the pane's: it takes the keyboard.
    pub(super) fn focus_screen_surface(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(surface) = cx.try_global::<Screens>().and_then(|s| s.tabs.get(&id)).cloned() {
            surface.update(cx, |surface, cx| surface.focus(cx));
        }
    }

    /// The surface's tab closed: its machine stops showing it.
    pub(super) fn close_screen_surface(&mut self, id: u64, cx: &mut Context<Self>) {
        let surface = cx
            .try_global::<Screens>()
            .is_some()
            .then(|| cx.global_mut::<Screens>().tabs.remove(&id))
            .flatten();
        if let Some(surface) = surface {
            surface.update(cx, |surface, cx| surface.close(cx));
        }
    }
}
