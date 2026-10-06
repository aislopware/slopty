//! The notices: another client's pointing, a closed tile to take back, a word to this client.
//! A notice about a tile's own work (a drop that did not land, a window that did not open)
//! sits beside that work, under its tile's header at its trailing edge, and moves with the
//! tile. Every other notice sits in the title bar, between where the focused work is and the
//! readouts: a lane no tile draws in, so it never lies over a composer's send button or a
//! shell's last rows, as one floating in the strip's corner did. On a phone, whose bar has no
//! such lane, they hang under the bar's middle. Each is one line, marked with what it is about
//! when it is about something, with at most its actions; they stay [`SAY_FOR`] and no more
//! than [`SHOWN`] are up at once in one place, side by side, the newest nearest the readouts.
//! Where there is no room for them all, the newest stays whole and the older go behind a count
//! that opens them under it, so none is cut off unseen. One whose time comes while the pointer is
//! over them stays until the pointer leaves, then [`SAY_AFTER_HOVER`] more, so a notice being read
//! is never taken away; one whose time comes while the app is not in front waits the same way for
//! it to come back, so nothing lapses unseen. A failure stays until it is dismissed, offering its
//! words to copy. A notice rises a hair into place as it fades in, and fades where it stands when
//! its time is up; under Reduce Motion it comes and goes at once.

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
use crate::icons::Symbol;

/// How long a pointing or a word stays up.
pub(super) const SAY_FOR: Duration = Duration::from_secs(6);

/// How long a notice whose time came under the pointer stays once the pointer leaves.
pub(super) const SAY_AFTER_HOVER: Duration = Duration::from_secs(2);

/// How many notices are up at once in one place: a third pushes the oldest out.
pub(super) const SHOWN: usize = 2;

/// The widest a notice gets, in points: past it the line ends in an ellipsis.
const TOAST_MAX_W: f32 = 400.0;

/// The least the newest notice narrows to, in ems of the chrome's text, before older ones in
/// its place have all gone behind the count.
const NEWEST_FLOOR_EM: f32 = 10.0;

/// The notices up now, oldest first. Made with the first notice and kept from then on.
#[derive(Default)]
pub(super) struct Toast {
    /// The last notice's number: it tells a stale dismiss timer from a live notice.
    seq: u64,
    shown: Vec<Shown>,
    /// The pointer is over the stack: no notice leaves meanwhile.
    hovered: bool,
    /// What each place's row left out at its last layout, for its count, and where its count
    /// was drawn.
    kept: std::cell::RefCell<std::collections::HashMap<Place, Kept>>,
    /// The place whose left-out notices are open under its count.
    opened: Option<Place>,
}

/// What a place's row keeps across frames.
#[derive(Default)]
struct Kept {
    /// What it left out.
    dropped: crate::kit::Dropped,
    /// Where its count was drawn: a press there toggles what it opened, so the click away
    /// leaves it be.
    count: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
}

/// Where notices are said: the title bar, or beside a tile (`Shown::at`).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Place(Option<TileRef>);

/// One notice up.
struct Shown {
    seq: u64,
    what: ToastKind,
    /// The tile whose work it is about, beside which it shows while the tile is in the
    /// layout; `None` for the title bar's.
    at: Option<TileRef>,
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
        self.show_toast_at(what, None, during, cx);
    }

    /// Show `what` for `during` beside `at`'s work, or in the title bar for `None`; the
    /// oldest in the same place goes when there would be more than [`SHOWN`] there.
    fn show_toast_at(
        &mut self,
        what: ToastKind,
        at: Option<TileRef>,
        during: Duration,
        cx: &mut Context<Self>,
    ) {
        let toast = self.toast.get_or_insert_with(Toast::default);
        toast.seq = toast.seq.wrapping_add(1);
        let seq = toast.seq;
        // One on its way out gives its place at once to the one coming in.
        toast.shown.retain(|shown| !shown.leaving);
        let sticky = matches!(what, ToastKind::Failed(_) | ToastKind::OldUnsaved(_));
        toast.shown.push(Shown { seq, what, at, leaving: false, held: false });
        // Past the most shown in its place, the oldest there goes, one that waits for an
        // answer only when nothing else is left to go.
        while toast.shown.iter().filter(|shown| shown.at == at).count() > SHOWN {
            let here = |shown: &&Shown| shown.at == at;
            let gone = toast
                .shown
                .iter()
                .enumerate()
                .filter(|(_, shown)| here(shown))
                .find(|(_, shown)| {
                    !matches!(shown.what, ToastKind::Failed(_) | ToastKind::OldUnsaved(_))
                })
                .or_else(|| toast.shown.iter().enumerate().find(|(_, shown)| here(shown)))
                .map(|(ix, _)| ix);
            let Some(gone) = gone else { break };
            toast.shown.remove(gone);
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
        let shown = &toast.shown;
        toast.kept.borrow_mut().retain(|at, _| shown.iter().any(|s| s.at == at.0));
        if toast.opened.is_some_and(|at| !shown.iter().any(|s| s.at == at.0)) {
            toast.opened = None;
        }
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

    /// A word about `tile`'s own work, said beside it.
    pub fn show_notice_at(&mut self, tile: TileRef, text: String, cx: &mut Context<Self>) {
        self.show_toast_at(ToastKind::Said(text), Some(tile), SAY_FOR, cx);
    }

    /// Something in `tile`'s own work that failed (a drop that did not land, a window that
    /// did not open), said beside it until dismissed.
    pub fn show_failure_at(&mut self, tile: TileRef, text: String, cx: &mut Context<Self>) {
        self.show_toast_at(ToastKind::Failed(text), Some(tile), SAY_FOR, cx);
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
            tab_stop(el, s.focus)
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
                    .map_or_else(|| Symbol::Xmark.into(), |c| self.kind_glyph(&c.item));
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
                ("offered", Some(Symbol::Globe.into()), vec![open, dismiss])
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
                ("old-unsaved", Some(Symbol::DocText.into()), vec![discard, keep])
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
                ("trashed", Some(Symbol::Trash.into()), vec![put_back])
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
            .h(px(theme.density.hit))
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .pl(px(theme.spacing.sm))
            .pr(px(if actions.is_empty() { theme.spacing.sm } else { theme.spacing.xxs }))
            // Raised in both variants: a notice is a thing on the bar or the tile, not one
            // more of its words.
            .map(|el| crate::kit::raised(el, theme))
            .rounded(px(theme.radii.sm))
            .text_color(hsla(s.text))
            .children(icon.map(|icon| {
                crate::icons::symbol(
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

    /// Whether `shown` is said beside its tile: one about a tile still in the layout. One
    /// whose tile has gone is said in the title bar.
    fn beside_tile(&self, shown: &Shown) -> Option<TileRef> {
        shown.at.filter(|tile| self.layout.contains(*tile))
    }

    /// The title bar's notices up now: the newest whole, nearest the readouts, and older ones
    /// beside it while the bar has room for them.
    pub(super) fn render_notices(&self, cx: &Draw<'_, Self>) -> Option<gpui::AnyElement> {
        self.notice_row(None, "notices", cx)
    }

    /// The notices about `tile`'s own work, for its trailing edge under its header, the newest
    /// nearest the edge.
    pub(super) fn render_tile_notices(
        &self,
        tile: TileRef,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        self.notice_row(Some(tile), "tile-notices", cx)
    }

    /// The notices up at `place` in a row named `selector`, painted over whatever is up there, a
    /// popover's click-away included; nothing when there are none. The row is as wide as its
    /// notices where there is room. Where there is not, older notices go behind a count ("+1")
    /// that opens them under it, and the newest, always shown, ends in an ellipsis.
    fn notice_row(
        &self,
        place: Option<TileRef>,
        selector: &'static str,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let toast = self.toast.as_ref()?;
        let here: Vec<&Shown> =
            toast.shown.iter().filter(|shown| self.beside_tile(shown) == place).collect();
        let (newest, older) = here.split_last()?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (dropped, count_at) = {
            let mut kept = toast.kept.borrow_mut();
            let kept = kept.entry(Place(place)).or_default();
            (kept.dropped.clone(), std::rc::Rc::clone(&kept.count))
        };
        let key = |shown: &Shown| SharedString::from(format!("notice-{}", shown.seq));
        let id = place.map_or_else(String::new, |t| t.item.as_uuid().to_string());
        let mut row = crate::kit::priority_row(SharedString::from(format!("notice-row-{id}")))
            .dropped(&dropped)
            .fit_content()
            .h(px(theme.density.hit))
            .gap(px(theme.spacing.xs))
            .title_priority(crate::kit::Priority::ESSENTIAL);
        // The older the notice, the sooner it goes behind the count.
        for (age, shown) in older.iter().enumerate() {
            let rank = u8::try_from(age).unwrap_or(u8::MAX);
            let priority = crate::kit::Priority(crate::kit::Priority::LOW.0.saturating_add(rank));
            row = row.item(key(shown), priority, self.render_one(shown, cx));
        }
        let floor = px(theme.typography.ui_size * NEWEST_FLOOR_EM);
        row = row.title(self.render_one(newest, cx), floor);
        let open = toast.opened == Some(Place(place)) && dropped.count() > 0;
        if !older.is_empty() {
            let count = dropped.count().max(1);
            let label = SharedString::from(format!("+{count}"));
            let aria = SharedString::from(format!("{count} more notices"));
            let pill = div()
                .id("notice-more")
                .debug_selector(move || format!("{selector}-more"))
                .role(Role::Button)
                .aria_label(aria)
                .aria_expanded(open)
                .h(px(theme.density.hit))
                .flex()
                .items_center()
                .px(px(theme.spacing.sm))
                .map(|el| crate::kit::raised(el, theme))
                .rounded(px(theme.radii.sm))
                .text_color(hsla(s.text_secondary))
                .font_weight(gpui::FontWeight(slopty_theme::Typography::MEDIUM_WEIGHT))
                .cursor_pointer()
                .occlude()
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    if let Some(toast) = this.toast.as_mut() {
                        let at = Place(place);
                        toast.opened = (toast.opened != Some(at)).then_some(at);
                        cx.notify();
                    }
                }))
                .child(label);
            let left: Vec<gpui::AnyElement> = older
                .iter()
                .filter(|shown| open && dropped.contains(&key(shown)))
                .map(|shown| self.render_one(shown, cx))
                .collect();
            let out_at = std::rc::Rc::clone(&count_at);
            let panel = (!left.is_empty()).then(|| {
                let stack = div()
                    .id("notice-left")
                    .debug_selector(move || format!("{selector}-left"))
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(theme.spacing.xs))
                    .font_family(theme.typography.ui_family.clone())
                    .text_size(px(theme.typography.small()))
                    .on_mouse_down_out(cx.listener(
                        move |this, ev: &gpui::MouseDownEvent, _w, cx| {
                            let on_count = out_at.get().is_some_and(|at| at.contains(&ev.position));
                            if let Some(toast) = this.toast.as_mut().filter(|_| !on_count) {
                                toast.opened = None;
                                cx.notify();
                            }
                        },
                    ))
                    .children(left);
                div().absolute().top_full().right_0().pt(px(theme.spacing.xs)).child(
                    gpui::deferred(
                        gpui::anchored()
                            .anchor(gpui::Anchor::TopRight)
                            .snap_to_window_with_margin(px(theme.spacing.sm))
                            .child(stack),
                    )
                    .with_priority(crate::palette::Layer::Toast.priority()),
                )
            });
            let count = div()
                .relative()
                .on_children_prepainted(move |bounds, _w, _cx| {
                    count_at.set(bounds.first().copied());
                })
                .child(tab_stop(pill, s.focus))
                .children(panel);
            row = row.menu(count);
        }
        let row = div()
            .debug_selector(move || selector.to_owned())
            .flex_initial()
            .min_w_0()
            .flex()
            .items_center()
            .font_family(theme.typography.ui_family.clone())
            .text_size(px(theme.typography.small()))
            .child(row);
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
