//! The one-line notice at the foot of the strip: another client's pointing, a closed tile to
//! take back, a word to this client.

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::{
    Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use slopty_client::layout::TileRef;
use slopty_proto::ClientMsg;
use slopty_theme::alpha;

use super::WorkspaceView;
use super::actions::PointOthers;
use crate::a11y::tab_stop;
use crate::colors::{hsla, hsla_alpha};

/// How long a pointing or a word stays up.
const SAY_FOR: Duration = Duration::from_secs(8);

/// The toast.
pub(super) struct Toast {
    /// Tells a stale dismiss timer from the current toast's.
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

    /// Show `what` for `during`: a newer toast replaces it and restarts the clock.
    pub(super) fn show_toast_for(
        &mut self,
        what: ToastKind,
        during: Duration,
        cx: &mut Context<Self>,
    ) {
        let seq = self.toast.as_ref().map_or(0, |t| t.seq).wrapping_add(1);
        self.toast = Some(Toast { seq, what });
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(during).await;
            let _gone = this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|t| t.seq == seq) {
                    this.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A word for the human (a picture refused, a worker added, settings that did not parse).
    pub fn show_notice(&mut self, text: String, cx: &mut Context<Self>) {
        self.show_toast(ToastKind::Said(text), cx);
    }

    /// The text of the toast up now, for tests and the self-test dump.
    #[must_use]
    pub fn toast_text(&self, cx: &gpui::App) -> Option<String> {
        Some(match &self.toast.as_ref()?.what {
            ToastKind::Pointed { name, tile } => {
                let item = self.item(*tile)?;
                format!("{name} points at {}", self.card_title(*tile, item, cx))
            }
            ToastKind::Closed { title, .. } => format!("Closed {title}"),
            ToastKind::Said(text) => text.clone(),
        })
    }

    /// The "closed" toast goes with its offer: `seq` for one closing, `None` for any.
    pub(super) fn dismiss_closed_toast(&mut self, seq: Option<u64>) {
        let closing = match &self.toast {
            Some(Toast { what: ToastKind::Closed { seq: s, .. }, .. }) => Some(*s),
            _ => None,
        };
        if closing.is_some_and(|s| seq.is_none_or(|seq| seq == s)) {
            self.toast = None;
        }
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

    /// The toast, centred at the foot of the strip. One surface for every kind: a toast is a
    /// notice that happens to be clickable, not a primary action, so the accent is only on
    /// the words that name what a click does. At the foot because the top of the strip is
    /// where the tiles' headers are, and a notice must not sit on the badge it is about.
    pub(super) fn render_toast(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let toast = self.toast.as_ref()?;
        let style = |d: Stateful<Div>| {
            d.flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .px(px(theme.spacing.md))
                .py(px(theme.spacing.xs))
                .rounded(px(theme.radii.md))
                .bg(hsla_alpha(theme.surfaces.panel, alpha::VEIL))
                .border_1()
                .border_color(hsla(theme.surfaces.border))
                .shadow_sm()
                .text_color(hsla(theme.surfaces.text))
                .text_size(px(theme.typography.small()))
                .font_family(theme.typography.ui_family.clone())
        };
        let offer = |text: &'static str| div().text_color(hsla(theme.surfaces.accent)).child(text);
        let inner = match &toast.what {
            ToastKind::Pointed { name, tile } => {
                let tile = *tile;
                let item = self.item(tile)?;
                let title = self.card_title(tile, item, cx);
                let pill = style(div().id("pointed"))
                    .debug_selector(|| "pointed".to_owned())
                    .role(Role::Button)
                    .aria_label(format!("{name} points at {title}, go there"))
                    .cursor_pointer()
                    .child(SharedString::from(format!("{name} points at {title}")))
                    .child(offer("· go"));
                tab_stop(pill, theme.surfaces.accent)
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.toast = None;
                        this.focus_tile(tile, cx);
                    }))
                    .into_any_element()
            }
            ToastKind::Closed { seq, title } => {
                let seq = *seq;
                let pill = style(div().id("closed"))
                    .debug_selector(|| "closed".to_owned())
                    .role(Role::Button)
                    .aria_label(format!("Closed {title}, undo"))
                    .cursor_pointer()
                    .child(SharedString::from(format!("Closed {title}")))
                    .child(offer("· Undo"));
                tab_stop(pill, theme.surfaces.accent)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.take_back(Some(seq), cx)))
                    .into_any_element()
            }
            ToastKind::Said(text) => style(div().id("said"))
                .debug_selector(|| "said".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(text.clone()))
                .child(SharedString::from(text.clone()))
                .into_any_element(),
        };
        Some(
            div()
                .absolute()
                .bottom(px(theme.spacing.lg))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(inner)
                .into_any_element(),
        )
    }
}
