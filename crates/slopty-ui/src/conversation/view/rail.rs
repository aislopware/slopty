//! The prompt rail: a tick per prompt down the list's right edge, at its place in the list; a
//! click goes there, the pointer on one shows its words.
//!
//! It is a view of its own, placed `cached`: its ticks are worked out when the prompts change,
//! not on every frame, and a frame that only scrolls the list or grows an answer reuses the rail
//! as it was drawn. Built on every frame, eighty prompts cost a headless panning frame about a
//! third of its time (`docs/MEASUREMENTS.md`, "the prompt rail off the face's frame").

use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::{
    AppContext as _, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div,
    px,
};
use slopty_theme::Theme;

use super::ConversationView;
use crate::colors::hsla;
use crate::kit;

/// Where a prompt sits in the list and the words its tick shows.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct Tick {
    pub ix: usize,
    pub hint: SharedString,
}

/// The rail's view.
pub(super) struct Rail {
    face: WeakEntity<ConversationView>,
    ticks: Rc<[Tick]>,
    /// The rows in the list: a tick's place is its row over these.
    total: usize,
    theme: Rc<Theme>,
    zoom: f32,
    /// How many times the rail has drawn.
    #[cfg(test)]
    renders: usize,
}

impl Rail {
    pub(super) fn new(face: WeakEntity<ConversationView>, theme: Rc<Theme>) -> Self {
        Self {
            face,
            ticks: Rc::from([]),
            total: 0,
            theme,
            zoom: 1.0,
            #[cfg(test)]
            renders: 0,
        }
    }

    /// Whether the rail shows: two prompts or more.
    pub(super) fn shown(&self) -> bool {
        self.ticks.len() >= 2
    }

    /// Draw these ticks over `total` rows, at this theme and zoom; nothing is drawn again when
    /// none of it changed.
    pub(super) fn set(
        &mut self,
        ticks: Option<Rc<[Tick]>>,
        total: usize,
        theme: &Rc<Theme>,
        zoom: f32,
        cx: &mut Context<Self>,
    ) {
        let mut changed = ticks.is_some();
        if let Some(ticks) = ticks {
            self.ticks = ticks;
        }
        if self.total != total
            || !Rc::ptr_eq(&self.theme, theme)
            || (self.zoom - zoom).abs() > f32::EPSILON
        {
            self.total = total;
            self.theme = Rc::clone(theme);
            self.zoom = zoom;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// How many times the rail has drawn.
    #[cfg(test)]
    pub(super) const fn renders(&self) -> usize {
        self.renders
    }

    fn z(&self, v: f32) -> gpui::Pixels {
        px(v * self.zoom)
    }
}

impl Render for Rail {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders = self.renders.saturating_add(1);
        }
        let theme = Rc::clone(&self.theme);
        let s = theme.surfaces;
        let total = self.total.max(1);
        let ticks = self.ticks.iter().enumerate().map(|(n, tick)| {
            #[expect(clippy::cast_precision_loss, reason = "a position on screen")]
            let at = tick.ix as f32 / total as f32;
            let (ix, hint) = (tick.ix, tick.hint.clone());
            let hint_theme = Rc::clone(&theme);
            let selector = format!("rail-{n}");
            div()
                .id(ElementId::Name(SharedString::from(selector.clone())))
                .debug_selector(move || selector)
                .role(Role::Button)
                .aria_label(SharedString::from(format!("Prompt {}: {hint}", n.saturating_add(1))))
                .absolute()
                .top(gpui::relative(at))
                .right_0()
                .w(self.z(theme.spacing.md))
                .h(self.z(theme.spacing.sm))
                .flex()
                .items_center()
                .justify_end()
                .cursor_pointer()
                .group("tick")
                .child(
                    div()
                        .w(self.z(theme.spacing.sm))
                        .h(px(2.0))
                        .rounded_full()
                        .bg(hsla(s.border))
                        .group_hover("tick", |el| {
                            el.bg(hsla(s.accent_fill)).w(self.z(theme.spacing.md))
                        }),
                )
                .tooltip(move |_window, cx| {
                    let (hint, theme) = (hint.clone(), Rc::clone(&hint_theme));
                    cx.new(|_| kit::Hint::new(hint, "", theme)).into()
                })
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    let _gone = this.face.update(cx, |face, cx| face.scroll_to_row(ix, cx));
                }))
        });
        div().debug_selector(|| "prompt-rail".to_owned()).size_full().children(ticks)
    }
}
