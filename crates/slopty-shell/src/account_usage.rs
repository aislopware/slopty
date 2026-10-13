// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! The composer footer's ring cluster: the context ring.
// slopty: no agent accounts: Slopty never touches a credential.
use gpui::{Context, Entity, IntoElement, Render, Subscription, Window, div, prelude::*, px};

use crate::popover;
use crate::state::AppState;
use crate::theme::Theme;

// slopty: no agent accounts: Slopty never touches a credential.
/// The footer's rings.
pub struct AccountUsage {
    state: Entity<AppState>,
    popup: popover::Popup<FooterCard>,
    _state: Subscription,
}

impl AccountUsage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        Self {
            _state: cx.observe(&state, |_, _, cx| cx.notify()),
            state,
            popup: popover::Popup::default(),
        }
    }

    // slopty: no agent accounts: Slopty never touches a credential.
    /// A trigger click: open this ring's popover, or close it when the
    /// press found it open (the card's mouse-down-out already began that
    /// close — see `Popup::note_trigger_press`).
    fn toggle(&mut self, card: FooterCard, cx: &mut Context<Self>) {
        if self.popup.take_press_was_open() || self.popup.as_open() == Some(&card) {
            self.dismiss(cx);
            return;
        }
        // slopty: no agent accounts: Slopty never touches a credential.
        self.popup.open(card);
        cx.notify();
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.popup.begin_close() {
            popover::reap_popup(cx, |usage: &mut Self| &mut usage.popup);
        }
        cx.notify();
    }

    /// A footer ring that opens `card` above itself, right-aligned like the
    /// model picker.
    fn trigger(
        &self,
        chip: gpui::Stateful<gpui::Div>,
        card: FooterCard,
        content: impl FnOnce(&Self, &mut Context<Self>) -> gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let chip = chip
            .relative()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |usage, _, _, _| {
                    usage
                        .popup
                        .note_trigger_press_matching(|open| *open == card);
                }),
            )
            .on_click(cx.listener(move |usage, _, _, cx| usage.toggle(card, cx)));
        if self.popup.get() != Some(&card) {
            return chip.into_any_element();
        }
        let content = content(self, cx)
            .on_mouse_down_out(cx.listener(|usage, _, _, cx| usage.dismiss(cx)))
            .into_any_element();
        chip.child(popover::anchored_menu_above_end(
            card.id(),
            content,
            self.popup.closing_since(),
        ))
        .into_any_element()
    }

    // slopty: no agent accounts: Slopty never touches a credential.
}

/// Which footer ring's popover is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FooterCard {
    // slopty: no agent accounts: Slopty never touches a credential.
    Context,
}

impl FooterCard {
    fn id(self) -> &'static str {
        match self {
            FooterCard::Context => "context-usage-menu",
        }
    }
}

/// The footer's ring cluster: context occupancy, opening its popover on click. // slopty: no agent accounts: Slopty never touches a credential.
impl Render for AccountUsage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let context = self.state.read(cx).context_usage;
        // slopty: no agent accounts: Slopty never touches a credential.
        let context = crate::context_usage::has_window(context).then(|| {
            let chip = crate::context_usage::chip(
                context,
                self.popup.get() == Some(&FooterCard::Context),
                &theme,
            );
            self.trigger(
                chip,
                FooterCard::Context,
                move |_, cx| crate::context_usage::card(context, &Theme::of(cx).for_popup()),
                cx,
            )
        });
        div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .children(context)
    }
}
