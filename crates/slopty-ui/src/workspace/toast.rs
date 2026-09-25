//! The notices in the strip's bottom-right corner: another client's pointing, a closed tile to
//! take back, a word to this client. Each is one line with an icon and at most one action;
//! they stay [`SAY_FOR`] and no more than [`SHOWN`] are up at once.

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use slopty_client::layout::TileRef;
use slopty_proto::ClientMsg;

use super::WorkspaceView;
use super::actions::PointOthers;
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::icons::{IconName, IconSize};

/// How long a pointing or a word stays up.
pub(super) const SAY_FOR: Duration = Duration::from_secs(6);

/// How many notices are up at once: a third pushes the oldest out.
pub(super) const SHOWN: usize = 2;

/// The widest a notice gets, in points: past it the line ends in an ellipsis.
const TOAST_MAX_W: f32 = 400.0;

/// The notices up now, oldest first. Made with the first notice and kept from then on.
#[derive(Default)]
pub(super) struct Toast {
    /// The last notice's number: it tells a stale dismiss timer from a live notice.
    seq: u64,
    shown: Vec<Shown>,
}

/// One notice up.
struct Shown {
    seq: u64,
    what: ToastKind,
}

/// What a toast is.
pub(super) enum ToastKind {
    /// Another client's pointing: a button that goes to the tile.
    Pointed {
        /// Who pointed, as they are named.
        name: String,
        /// At what.
        tile: TileRef,
    },
    /// A tile just closed: a button that takes it back until the offer lapses.
    Closed {
        /// Which closing (`ClosedTile::seq`).
        seq: u64,
        /// The tile's title as it was.
        title: String,
    },
    /// A word to this client alone.
    Said(String),
}

impl WorkspaceView {
    pub(super) fn show_toast(&mut self, what: ToastKind, cx: &mut Context<Self>) {
        self.show_toast_for(what, SAY_FOR, cx);
    }

    /// Show `what` for `during`, under the notices already up; the oldest goes when there
    /// would be more than [`SHOWN`].
    pub(super) fn show_toast_for(
        &mut self,
        what: ToastKind,
        during: Duration,
        cx: &mut Context<Self>,
    ) {
        let toast = self.toast.get_or_insert_with(Toast::default);
        toast.seq = toast.seq.wrapping_add(1);
        let seq = toast.seq;
        toast.shown.push(Shown { seq, what });
        let over = toast.shown.len().saturating_sub(SHOWN);
        toast.shown.drain(..over);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(during).await;
            let _gone = this.update(cx, |this, cx| {
                if this.drop_toasts(|shown| shown.seq == seq) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Take down the notices `which` picks; `true` when one went.
    fn drop_toasts(&mut self, which: impl Fn(&Shown) -> bool) -> bool {
        let Some(toast) = self.toast.as_mut() else { return false };
        let before = toast.shown.len();
        toast.shown.retain(|shown| !which(shown));
        // The numbering carries on when the last notice goes, so a timer left from before
        // can never take down a newer notice that happens to reuse its number.
        toast.shown.len() != before
    }

    /// A word for the human (a picture refused, a worker added, settings that did not parse).
    pub fn show_notice(&mut self, text: String, cx: &mut Context<Self>) {
        self.show_toast(ToastKind::Said(text), cx);
    }

    /// What a notice says.
    fn toast_line(&self, what: &ToastKind, cx: &gpui::App) -> Option<String> {
        Some(match what {
            ToastKind::Pointed { name, tile } => {
                let item = self.item(*tile)?;
                format!("{name} points at {}", self.card_title(*tile, item, cx))
            }
            ToastKind::Closed { title, .. } => format!("Closed {title}"),
            ToastKind::Said(text) => text.clone(),
        })
    }

    /// The text of the newest notice up now, for tests and the self-test dump.
    #[must_use]
    pub fn toast_text(&self, cx: &gpui::App) -> Option<String> {
        let shown = self.toast.as_ref()?.shown.last()?;
        self.toast_line(&shown.what, cx)
    }

    /// The texts of every notice up now, oldest first.
    #[must_use]
    pub fn toast_texts(&self, cx: &gpui::App) -> Vec<String> {
        self.toast.as_ref().map_or_else(Vec::new, |t| {
            t.shown.iter().filter_map(|s| self.toast_line(&s.what, cx)).collect()
        })
    }

    /// The "closed" toast goes with its offer: `seq` for one closing, `None` for any.
    pub(super) fn dismiss_closed_toast(&mut self, seq: Option<u64>) {
        self.drop_toasts(|shown| match shown.what {
            ToastKind::Closed { seq: s, .. } => seq.is_none_or(|seq| seq == s),
            _ => false,
        });
    }

    /// ⌘⇧O: point the other clients of the focused tile's worker at it. The worker relays it;
    /// this client's own echo is nothing.
    pub fn point_others(&mut self, _: &PointOthers, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tile) = self.focused() {
            self.point_at(tile, cx);
        }
    }

    pub(super) fn point_at(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        let Some(title) = self.item(tile).map(|i| self.card_title(tile, i, cx)) else { return };
        self.send(tile.worker, ClientMsg::Point { item: tile.item });
        self.show_toast(ToastKind::Said(format!("Pointed the others at {title}")), cx);
    }

    /// One notice: an icon for what it is about, its line, and its one action. The action is
    /// the only accent: a notice is not a primary action, only a way to one.
    fn render_one(&self, shown: &Shown, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let line = self.toast_line(&shown.what, cx)?;
        let action = |id: &'static str, label: &'static str| {
            let el = div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .px(px(theme.spacing.sm))
                .py(px(theme.spacing.xxs))
                .rounded(px(theme.radii.sm))
                .text_color(hsla(s.accent))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.raised)))
                .child(label);
            tab_stop(el, s.accent)
        };
        let (part, icon, action) = match &shown.what {
            ToastKind::Pointed { tile, .. } => {
                let tile = *tile;
                let go =
                    action("toast-go", "Go").on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.dismiss_pointed(tile);
                        this.focus_tile(tile, cx);
                    }));
                ("pointed", IconName::MousePointer2, Some(go))
            }
            ToastKind::Closed { seq, .. } => {
                let seq = *seq;
                // The closed tile's kind, so the notice names what went as the header did.
                let icon = self
                    .closed
                    .iter()
                    .find(|c| c.seq == seq)
                    .map_or(IconName::X, |c| super::tile::kind_icon(&c.item, false));
                let undo = action("toast-undo", "Undo")
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.take_back(Some(seq), cx)));
                ("closed", icon, Some(undo))
            }
            ToastKind::Said(_) => ("said", IconName::Info, None),
        };
        let line = SharedString::from(line);
        Some(
            div()
                .id(("toast", shown.seq))
                .debug_selector(move || part.to_owned())
                .role(Role::Status)
                .aria_label(line.clone())
                .occlude()
                .max_w(px(TOAST_MAX_W))
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(theme.spacing.sm))
                .pl(px(theme.spacing.md))
                .pr(px(if action.is_some() { theme.spacing.xs } else { theme.spacing.md }))
                .py(px(theme.spacing.xs))
                .rounded(px(theme.radii.md))
                .bg(hsla(s.panel))
                .border_1()
                .border_color(hsla(s.border))
                .shadow_sm()
                .text_color(hsla(s.text))
                .text_size(px(theme.typography.small()))
                .font_family(theme.typography.ui_family.clone())
                .child(crate::icons::icon(theme, icon, IconSize::Inline, hsla(s.text_secondary)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(line),
                )
                .when_some(action, gpui::ParentElement::child)
                .into_any_element(),
        )
    }

    /// A pointing at `tile` has been followed.
    fn dismiss_pointed(&mut self, tile: TileRef) {
        self.drop_toasts(
            |shown| matches!(shown.what, ToastKind::Pointed { tile: t, .. } if t == tile),
        );
    }

    /// The notices, stacked up from the strip's bottom-right corner, the newest lowest. In the
    /// corner because the top of the strip is where the tiles' headers are, and the middle of
    /// its foot is where a tile's own state pill sits.
    pub(super) fn render_toast(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let toast = self.toast.as_ref()?;
        let notices: Vec<gpui::AnyElement> =
            toast.shown.iter().filter_map(|shown| self.render_one(shown, cx)).collect();
        if notices.is_empty() {
            return None;
        }
        let drawn = std::rc::Rc::clone(&self.toast_drawn);
        let frame = self.frames_drawn;
        // Where the notices are, for a browser tile's page to stop above them.
        let measure = gpui::canvas(
            move |bounds, _window, _cx| drawn.set(Some((frame, bounds))),
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .inset_0();
        Some(
            div()
                .absolute()
                .bottom(px(theme.spacing.lg))
                .right(px(theme.spacing.lg))
                .max_w(px(TOAST_MAX_W))
                .flex()
                .flex_col()
                .items_end()
                .gap(px(theme.spacing.sm))
                .children(notices)
                .child(measure)
                .into_any_element(),
        )
    }
}
