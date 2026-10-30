//! The remote window and desktop views ([`RightSurface::Screen`]), summoned beside a
//! session as the browser is: the palette's command, the title, the body and the close, each a
//! call from the shell's own match on the view.

use std::collections::HashMap;

use gpui::{AnyElement, App, AppContext as _, Context, Empty, Entity, Global, IntoElement, SharedString};

use super::{RightSurface, Shell};
use crate::live::screen::ScreenSurface;

/// The screen views, by their number.
#[derive(Default)]
struct Screens {
    seq: u64,
    tabs: HashMap<u64, Entity<ScreenSurface>>,
}

impl Global for Screens {}

impl Shell {
    /// The palette's "Remote window": a new view beside the session listing its machine's
    /// desktops and windows.
    pub(super) fn add_screen_surface(&mut self, cx: &mut Context<Self>) {
        let state = self.state.clone();
        let surface = cx.new(|cx| ScreenSurface::new(state, cx));
        let screens = cx.default_global::<Screens>();
        screens.seq += 1;
        let id = screens.seq;
        screens.tabs.insert(id, surface);
        self.open_view(RightSurface::Screen(id), cx);
    }

    /// The view showing again `screen`, titled `title`, which the session had beside it when the
    /// app last ran.
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
        self.open_view(RightSurface::Screen(id), cx);
        RightSurface::Screen(id)
    }

    /// What the surface was chosen to show, its id and title, to show it again.
    pub(super) fn screen_surface_chosen(&self, id: u64, cx: &App) -> Option<(String, String)> {
        cx.try_global::<Screens>()?.tabs.get(&id)?.read(cx).chosen()
    }

    /// The view's title: its window's.
    pub(super) fn screen_surface_title(&self, id: u64, cx: &App) -> Option<SharedString> {
        Some(cx.try_global::<Screens>()?.tabs.get(&id)?.read(cx).title())
    }

    /// The view's body, nothing once it is gone.
    pub(super) fn render_screen_surface(&mut self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        match cx.try_global::<Screens>().and_then(|s| s.tabs.get(&id)).cloned() {
            Some(surface) => surface.into_any_element(),
            None => Empty.into_any_element(),
        }
    }

    /// The view came forward: it takes the keyboard.
    pub(super) fn focus_screen_surface(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(surface) = cx.try_global::<Screens>().and_then(|s| s.tabs.get(&id)).cloned() {
            surface.update(cx, |surface, cx| surface.focus(cx));
        }
    }

    /// The view closed: its machine stops showing it.
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
