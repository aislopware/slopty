//! The notices: another client's pointing, a closed tile to take back, a word to this client.
//! They sit in the status bar, between where the focused tile runs and the bar's readouts: a
//! lane no tile draws in, so a notice never lies over a composer's send button or a shell's
//! last rows, as one floating in the strip's corner did. Each is one line, marked with what it
//! is about when it is about something, with at most its actions; they stay [`SAY_FOR`] and no
//! more than [`SHOWN`] are up at once, side by side, the newest nearest the readouts. One whose
//! time comes while the pointer is over them stays until the pointer leaves, then
//! [`SAY_AFTER_HOVER`] more, so a notice being read is never taken away; one whose time comes
//! while the app is not in front waits the same way for it to come back, so nothing lapses
//! unseen. A failure stays until it is dismissed, offering its words to copy. A notice rises a
//! hair into place as it fades in, and fades where it stands when its time is up; under Reduce
//! Motion it comes and goes at once.

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, Context, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_client::layout::TileRef;

use super::WorkspaceView;
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{Glyph, IconName};

/// How long a pointing or a word stays up.
pub(super) const SAY_FOR: Duration = Duration::from_secs(6);

/// How long a notice whose time came under the pointer stays once the pointer leaves.
pub(super) const SAY_AFTER_HOVER: Duration = Duration::from_secs(2);

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
    /// The pointer is over the stack: no notice leaves meanwhile.
    hovered: bool,
}

/// One notice up.
struct Shown {
    seq: u64,
    what: ToastKind,
    /// Its time is up and it is fading out: no longer counted as up.
    leaving: bool,
    /// Its time came while the pointer was over the stack: it goes once the pointer leaves.
    held: bool,
}

/// What a toast is.
pub(super) enum ToastKind {
    /// A tile just closed: a button that takes it back until the offer lapses.
    Closed {
        /// Which closing (`ClosedTile::seq`).
        seq: u64,
        /// The tile's title as it was.
        title: String,
    },
    /// A word to this client alone.
    Said(String),
    /// Something that failed: it stays until dismissed, in the error's tone, and offers its
    /// words to copy.
    Failed(String),
    /// A page a program in a shell asked to open, held back: "Open" opens it.
    Offered(Box<super::handoffs::Offer>),
    /// Unsaved edits kept on this device have waited over a week for their machines, said at
    /// start: "Discard" lets them go, "Keep" keeps them waiting. It stays until one is chosen.
    OldUnsaved(String),
    /// An entry went to a worker's trash: "Put back" moves it back where it was.
    Trashed {
        /// The worker.
        worker: slopty_client::layout::WorkerKey,
        /// The move that puts it back.
        back: slopty_proto::folder::FsOp,
        /// What happened.
        line: String,
    },
    /// A tile off screen came to need the person while the app is in front: "Go" goes to it. A
    /// pointer, never an answer: it carries no Allow, Deny or choice.
    Attention {
        /// Which.
        tile: TileRef,
        /// Its state, whose mark leads the line.
        status: crate::icons::Status,
        /// What happened there, led by the tile's name.
        line: String,
    },
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
        // One on its way out gives its place at once to the one coming in.
        toast.shown.retain(|shown| !shown.leaving);
        let sticky = matches!(what, ToastKind::Failed(_) | ToastKind::OldUnsaved(_));
        toast.shown.push(Shown { seq, what, leaving: false, held: false });
        // Past the most shown, the oldest goes, one that waits for an answer only when nothing
        // else is left to go.
        while toast.shown.len() > SHOWN {
            let at = toast
                .shown
                .iter()
                .position(|shown| {
                    !matches!(shown.what, ToastKind::Failed(_) | ToastKind::OldUnsaved(_))
                })
                .unwrap_or(0);
            toast.shown.remove(at);
        }
        cx.notify();
        if !sticky {
            Self::arm_toast(seq, during, cx);
        }
    }

    /// Take notice `seq` down `during` from now, unless the pointer is over the stack then.
    fn arm_toast(seq: u64, during: Duration, cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(during).await;
            let fades = this
                .update(cx, |this, cx| {
                    if this.hold_toast(seq) {
                        return false;
                    }
                    let fades = this.chrome_moves(cx);
                    let went = if fades {
                        this.leave_toast(seq)
                    } else {
                        this.drop_toasts(|shown| shown.seq == seq)
                    };
                    if went {
                        cx.notify();
                    }
                    fades && went
                })
                .unwrap_or(false);
            if fades {
                cx.background_executor().timer(crate::kit::Pace::Fade.duration()).await;
                let _gone = this.update(cx, |this, cx| {
                    if this.drop_toasts(|shown| shown.seq == seq) {
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Whether notice `seq` stays because the pointer is over the stack or the app is not in
    /// front; it is marked to go once the pointer leaves and the app is back.
    fn hold_toast(&mut self, seq: u64) -> bool {
        let away = !self.app_active;
        let Some(toast) = self.toast.as_mut() else { return false };
        if !toast.hovered && !away {
            return false;
        }
        if let Some(shown) = toast.shown.iter_mut().find(|s| s.seq == seq) {
            shown.held = true;
        }
        true
    }

    /// The pointer came over the notices or left them: leaving, each held one gets
    /// [`SAY_AFTER_HOVER`] more.
    pub(super) fn toast_hovered(&mut self, hovered: bool, cx: &Context<Self>) {
        let Some(toast) = self.toast.as_mut() else { return };
        toast.hovered = hovered;
        if !hovered {
            self.release_held_toasts(cx);
        }
    }

    /// The pointer left the notices or the app came back to the front: each notice held
    /// meanwhile gets [`SAY_AFTER_HOVER`] more, unless the other still holds it.
    pub(super) fn release_held_toasts(&mut self, cx: &Context<Self>) {
        let active = self.app_active;
        let Some(toast) = self.toast.as_mut() else { return };
        if toast.hovered || !active {
            return;
        }
        let held: Vec<u64> = toast
            .shown
            .iter_mut()
            .filter_map(|s| std::mem::take(&mut s.held).then_some(s.seq))
            .collect();
        for seq in held {
            Self::arm_toast(seq, SAY_AFTER_HOVER, cx);
        }
    }

    /// Start notice `seq` fading out; `true` when it was up.
    fn leave_toast(&mut self, seq: u64) -> bool {
        let Some(toast) = self.toast.as_mut() else { return false };
        let Some(shown) = toast.shown.iter_mut().find(|s| s.seq == seq && !s.leaving) else {
            return false;
        };
        shown.leaving = true;
        true
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

    /// A word for the human (a picture refused, a worker added).
    pub fn show_notice(&mut self, text: String, cx: &mut Context<Self>) {
        self.show_toast(ToastKind::Said(text), cx);
    }

    /// Something that failed (settings that did not parse, a save that did not land): it stays
    /// until dismissed and offers its words to copy.
    pub fn show_failure(&mut self, text: String, cx: &mut Context<Self>) {
        self.show_toast(ToastKind::Failed(text), cx);
    }

    /// Say an entry went to `worker`'s trash, with "Put back", which asks for `back`.
    pub(super) fn show_trashed(
        &mut self,
        worker: slopty_client::layout::WorkerKey,
        line: String,
        back: slopty_proto::folder::FsOp,
        cx: &mut Context<Self>,
    ) {
        self.show_toast(ToastKind::Trashed { worker, back, line }, cx);
    }

    /// The text of the newest notice up now, for tests and the self-test dump.
    #[must_use]
    pub fn toast_text(&self) -> Option<String> {
        let shown = self.toast.as_ref()?.shown.iter().rev().find(|shown| !shown.leaving)?;
        Some(toast_line(&shown.what))
    }

    /// The "closed" toast goes with its offer: `seq` for one closing, `None` for any.
    pub(super) fn dismiss_closed_toast(&mut self, seq: Option<u64>) {
        self.drop_toasts(|shown| match shown.what {
            ToastKind::Closed { seq: s, .. } => seq.is_none_or(|seq| seq == s),
            _ => false,
        });
    }

    /// One notice: a mark for what it is about, its line, and its one action. The action is a
    /// ghost in the medium weight, not the accent: a notice is not a primary action, only a
    /// way to one, and the green means live or done.
    fn render_one(&self, shown: &Shown, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let line = toast_line(&shown.what);
        let action = |id: &'static str, label: &'static str| {
            let el = div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .px(px(theme.spacing.xs))
                .rounded(px(theme.radii.xs))
                .text_color(hsla(s.text))
                .font_weight(gpui::FontWeight(slopty_theme::Typography::MEDIUM_WEIGHT))
                .cursor_pointer()
                .hover(gpui::Styled::underline)
                .child(label);
            tab_stop(el, s.accent)
        };
        let mut body = None;
        let mut mark = None;
        let (part, icon, actions) = match &shown.what {
            ToastKind::Closed { seq, .. } => {
                let seq = *seq;
                // The closed tile's kind, so the notice names what went as the header did.
                let icon = self
                    .closed
                    .iter()
                    .find(|c| c.seq == seq)
                    .map_or(Glyph::Icon(IconName::X), |c| self.kind_glyph(&c.item));
                let undo = action("toast-undo", "Undo")
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.take_back(Some(seq), cx)));
                ("closed", Some(icon), vec![undo])
            }
            // A word needs no mark: an info glyph on every notice says nothing the line does not.
            ToastKind::Said(_) => ("said", None, Vec::new()),
            ToastKind::Failed(text) => {
                let (words, seq) = (text.clone(), shown.seq);
                let copy = action("toast-copy", "Copy").on_click(cx.listener(
                    move |_this, _ev, _w, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(words.clone()));
                    },
                ));
                let dismiss = action("toast-dismiss", "Dismiss")
                    .text_color(hsla(s.text_secondary))
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        if this.drop_toasts(|shown| shown.seq == seq) {
                            cx.notify();
                        }
                    }));
                mark =
                    Some(crate::icons::status_mark(theme, Some(crate::icons::Status::Failed), 1.0));
                ("failed", None, vec![copy, dismiss])
            }
            ToastKind::Offered(offer) => {
                let (worker, id, url) = offer.target();
                let open =
                    action("toast-open", "Open").on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.open_offered(worker, id, &url, cx);
                    }));
                let dismiss = action("toast-dismiss", "Dismiss")
                    .text_color(hsla(s.text_secondary))
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.dismiss_offer(worker, id);
                        cx.notify();
                    }));
                body = Some(self.offer_body(offer));
                ("offered", Some(Glyph::Icon(IconName::Globe)), vec![open, dismiss])
            }
            ToastKind::OldUnsaved(_) => {
                let seq = shown.seq;
                let discard = action("toast-discard", "Discard").on_click(cx.listener(
                    move |this, _ev, _w, cx| {
                        this.drop_toasts(|shown| shown.seq == seq);
                        this.discard_old_unsaved(cx);
                    },
                ));
                let keep = action("toast-keep", "Keep")
                    .text_color(hsla(s.text_secondary))
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        if this.drop_toasts(|shown| shown.seq == seq) {
                            cx.notify();
                        }
                    }));
                ("old-unsaved", Some(Glyph::Icon(IconName::FileText)), vec![discard, keep])
            }
            ToastKind::Trashed { worker, back, .. } => {
                let (worker, back, seq) = (*worker, back.clone(), shown.seq);
                let put_back = action("toast-put-back", "Put back").on_click(cx.listener(
                    move |this, _ev, _w, cx| {
                        this.drop_toasts(|shown| shown.seq == seq);
                        this.fs_op(worker, back.clone(), cx);
                        cx.notify();
                    },
                ));
                ("trashed", Some(Glyph::Icon(IconName::Trash)), vec![put_back])
            }
            ToastKind::Attention { tile, status, .. } => {
                let tile = *tile;
                let go =
                    action("toast-go", "Go").on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.dismiss_attention(tile);
                        this.focus_tile(tile, cx);
                    }));
                mark = Some(crate::icons::status_mark(theme, Some(*status), 1.0));
                ("attention", None, vec![go])
            }
        };
        let line = SharedString::from(line);
        let notice = div()
            .id(("toast", shown.seq))
            .debug_selector(move || part.to_owned())
            .role(Role::Status)
            .aria_label(line.clone())
            // Each notice hears the pointer: it occludes what is under it, the stack included.
            // A key pressed under a resting pointer is not the pointer leaving: the default
            // mode reads it so, and a notice being read would start its countdown.
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                this.toast_hovered(*hovered, cx);
            }))
            .hover_listener_mode(gpui::HoverListenerMode::InputModalityIndependent)
            .occlude()
            .max_w(px(TOAST_MAX_W))
            .min_w_0()
            .h(px(theme.spacing.xxs.mul_add(-2.0, super::statusbar::STATUSBAR_H)))
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .pl(px(theme.spacing.sm))
            .pr(px(if actions.is_empty() { theme.spacing.sm } else { theme.spacing.xxs }))
            // Raised off the bar in both variants: a notice is a thing on the bar, not one
            // more of its readouts.
            .map(|el| crate::kit::raised(el, theme))
            .rounded(px(theme.radii.sm))
            .text_color(hsla(s.text))
            .children(icon.map(|icon| {
                crate::icons::glyph(
                    theme,
                    icon,
                    px(theme.typography.icon()),
                    hsla(s.text_secondary),
                )
            }))
            .children(mark)
            .child(body.unwrap_or_else(|| {
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(line)
                    .into_any_element()
            }))
            .children(actions);
        if !self.chrome_moves(cx) {
            return notice.into_any_element();
        }
        if shown.leaving {
            let out = Animation::new(crate::kit::Pace::Fade.duration())
                .with_easing(crate::kit::ease_out());
            return notice
                .with_animation(("toast-out", shown.seq), out, |el, t| el.opacity(1.0 - t))
                .into_any_element();
        }
        let rise = theme.spacing.xxs;
        crate::kit::slide_fade(notice, ("toast-in", shown.seq), rise, crate::kit::Pace::Fade, cx)
    }

    /// Take down the held-back pages `which` picks.
    pub(super) fn drop_offers(&mut self, which: impl Fn(&super::handoffs::Offer) -> bool) {
        self.drop_toasts(|shown| matches!(&shown.what, ToastKind::Offered(offer) if which(offer)));
    }

    /// A word about `tile`, which came to need the person, when the app is in front and the
    /// tile is off screen: in view it says it itself, and away the system's note does. It
    /// replaces the one about the same tile; the stack holds two at most. Failures and finishes
    /// never come here: the tiles' marks and the bell hold them.
    pub(super) fn attention_toast(
        &mut self,
        tile: TileRef,
        status: crate::icons::Status,
        what: &str,
        cx: &mut Context<Self>,
    ) {
        if !self.app_active || self.drawn.on_screen.borrow().contains(&tile.item) {
            return;
        }
        let Some(title) = self.item(tile).map(|i| self.tile_title(i)) else { return };
        self.dismiss_attention(tile);
        let line = format!("{title} {what}");
        self.show_toast(ToastKind::Attention { tile, status, line }, cx);
    }

    /// The word about `tile` has been followed, or a newer one replaces it.
    fn dismiss_attention(&mut self, tile: TileRef) {
        self.drop_toasts(
            |shown| matches!(shown.what, ToastKind::Attention { tile: t, .. } if t == tile),
        );
    }

    /// Take down the words about tiles that no longer need the person: answered anywhere, in
    /// the tile, the inbox, a note or another client. `true` when one went.
    pub(super) fn drop_answered_attention(&mut self) -> bool {
        let up = self
            .toast
            .as_ref()
            .is_some_and(|t| t.shown.iter().any(|s| matches!(s.what, ToastKind::Attention { .. })));
        if !up {
            return false;
        }
        let needing: std::collections::HashSet<TileRef> = self
            .needs_you()
            .into_iter()
            .filter_map(|w| w.tile)
            .chain(self.threads_waiting().into_iter().filter_map(|w| w.tile))
            .collect();
        self.drop_toasts(
            |shown| matches!(shown.what, ToastKind::Attention { tile, .. } if !needing.contains(&tile)),
        )
    }

    /// Whether a notice is up, which keeps the status bar up to hold it.
    pub(super) fn notices_up(&self) -> bool {
        self.toast.as_ref().is_some_and(|t| !t.shown.is_empty())
    }

    /// The notices up now, side by side for the status bar, the newest nearest its readouts.
    pub(super) fn render_notices(&self, cx: &Draw<'_, Self>) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let toast = self.toast.as_ref()?;
        let notices: Vec<gpui::AnyElement> =
            toast.shown.iter().map(|shown| self.render_one(shown, cx)).collect();
        if notices.is_empty() {
            return None;
        }
        let row = div()
            .debug_selector(|| "notices".to_owned())
            .flex_initial()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .children(notices);
        // Laid out in the bar, painted over whatever is up there, a popover's click-away
        // included.
        Some(
            gpui::deferred(row)
                .with_priority(crate::palette::Layer::Toast.priority())
                .into_any_element(),
        )
    }
}

#[cfg(test)]
impl WorkspaceView {
    /// The hosts of the held-back pages up now, oldest first.
    #[must_use]
    pub(super) fn offered_hosts(&self) -> Vec<String> {
        self.toast
            .iter()
            .flat_map(|t| &t.shown)
            .filter(|s| !s.leaving)
            .filter_map(|s| match &s.what {
                ToastKind::Offered(offer) => Some(offer.host().to_owned()),
                _ => None,
            })
            .collect()
    }

    /// The texts of every notice up now, oldest first.
    #[must_use]
    pub(super) fn toast_texts(&self) -> Vec<String> {
        self.toast.as_ref().map_or_else(Vec::new, |t| {
            t.shown.iter().filter(|s| !s.leaving).map(|s| toast_line(&s.what)).collect()
        })
    }
}

/// What a notice says.
fn toast_line(what: &ToastKind) -> String {
    match what {
        ToastKind::Closed { title, .. } => format!("Closed {title}"),
        ToastKind::Said(text) | ToastKind::Failed(text) | ToastKind::OldUnsaved(text) => {
            text.clone()
        }
        ToastKind::Offered(offer) => offer.line(),
        ToastKind::Attention { line, .. } | ToastKind::Trashed { line, .. } => line.clone(),
    }
}
