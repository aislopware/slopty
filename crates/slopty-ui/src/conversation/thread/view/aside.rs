//! "Ask aside": a question beside the work, asked of a fork of the whole thread that shows in a
//! sheet over it and is gone when the sheet closes, unless the person keeps it. It is one of
//! "Branch from here"'s choices (`super::branch`), and in the palette.
//!
//! The worker forks the thread for it (`Intent::Aside`, where the agent forks, `Cap::FORK`)
//! and marks the fork as an aside of this one (`ThreadMeta::aside_of`), so it stays out of the
//! lists that show threads. The question is the draft, sent to the fork once it is there; with
//! no draft the sheet opens on the fork's own composer. The sheet holds the fork's own thread
//! view, without its header, so its answers, steps and requests read as anywhere else.
//!
//! Closing the sheet ends the fork for good (`Intent::Discard`): its agent, its thread and its
//! log, though never the agent's own session. "Keep as a thread" drops the mark
//! (`Intent::KeepAside`), and once the worker says so the workspace opens it as a thread of its
//! own (`HubEvent::Started`).

use gpui::accesskit::Role;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    relative,
};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{Cap, IntentId, ThreadId};

use super::{ThreadView, ThreadViewEvent};
use crate::colors::hsla;
use crate::icons::IconName;
use crate::kit::{self, ButtonKind};

/// The share of the thread's height the sheet takes.
const SHEET: f32 = 0.55;

/// An aside asked from this view.
pub(super) struct Aside {
    /// The `Intent::Aside` that forks it.
    intent: IntentId,
    /// The question, sent to the fork once it is there; none when the sheet opens on its
    /// composer.
    question: Option<String>,
    /// The fork, once the worker made it.
    thread: Option<ThreadId>,
    /// Its view in the sheet, made on the first frame after the fork came.
    view: Option<Entity<ThreadView>>,
    /// The person closed the sheet before the fork came: it is ended as it comes.
    closed: bool,
}

impl Aside {
    /// The fork's view in the sheet, once made.
    pub(super) fn view(&self) -> Option<Entity<ThreadView>> {
        self.view.clone()
    }
}

impl ThreadView {
    /// Whether an aside can be asked here: the agent forks, and this is not an aside itself.
    pub(super) fn can_aside(&self, cx: &gpui::App) -> bool {
        self.state(cx).is_some_and(|st| st.meta.can(Cap::FORK) && st.meta.aside_of().is_none())
    }

    /// The fork the sheet shows, once there.
    #[must_use]
    pub fn aside(&self) -> Option<ThreadId> {
        self.aside.as_ref().filter(|a| !a.closed).and_then(|a| a.thread)
    }

    /// Ask the draft aside: fork the thread, and send the draft to the fork once it is there.
    pub(super) fn ask_aside(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.aside.as_ref().filter(|a| !a.closed).and_then(|a| a.view.clone()) {
            view.update(cx, |v, cx| v.focus(window, cx));
            return;
        }
        if !self.can_aside(cx) {
            return;
        }
        let draft = self.draft(cx);
        let question = Some(draft.trim().to_owned()).filter(|q| !q.is_empty());
        if question.is_some() {
            self.composer.update(cx, |c, cx| c.clean(window, cx));
        }
        let intent = self.intent(Intent::Aside, cx);
        self.aside = Some(Aside { intent, question, thread: None, view: None, closed: false });
        cx.notify();
    }

    /// The worker forked the thread for `intent`: the question goes to `thread`, or the fork
    /// ends at once if the sheet was closed meanwhile.
    pub(super) fn aside_started(
        &mut self,
        intent: IntentId,
        thread: ThreadId,
        cx: &mut Context<Self>,
    ) {
        let Some(aside) = self.aside.as_mut().filter(|a| a.intent == intent) else { return };
        if aside.closed {
            self.aside = None;
            self.hub.update(cx, |hub, cx| hub.intent(thread, Intent::Discard, cx));
            return;
        }
        aside.thread = Some(thread);
        if let Some(text) = aside.question.take() {
            let delivery = self.send_now(cx);
            let send = Intent::Send { text, delivery, attachments: Vec::new() };
            self.hub.update(cx, |hub, cx| hub.intent(thread, send, cx));
        }
        cx.notify();
    }

    /// Close the sheet and end the fork for good. Whether one was open.
    pub(super) fn close_aside(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(aside) = self.aside.as_mut().filter(|a| !a.closed) else { return false };
        if let Some(thread) = aside.thread {
            self.aside = None;
            self.hub.update(cx, |hub, cx| hub.intent(thread, Intent::Discard, cx));
        } else {
            aside.closed = true;
            aside.view = None;
        }
        cx.notify();
        true
    }

    /// Keep the fork as a thread of its own: the mark goes, and the workspace opens it.
    fn keep_aside(&mut self, cx: &mut Context<Self>) {
        let Some(thread) = self.aside() else { return };
        self.aside = None;
        self.hub.update(cx, |hub, cx| hub.intent(thread, Intent::KeepAside, cx));
        cx.notify();
    }

    /// The sheet over the thread's foot while an aside is open: its head, and the fork's own
    /// thread view.
    pub(super) fn aside_sheet(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let aside = self.aside.as_ref().filter(|a| !a.closed)?;
        let made = aside.view.is_some();
        if let (Some(thread), false) = (aside.thread, made) {
            let (hub, theme) = (self.hub.clone(), self.theme.clone());
            let (zoom, width) = (self.zoom, self.width);
            let asked = aside.question.is_none();
            let view = cx.new(|cx| {
                let mut view = Self::new(hub, thread, theme, window, cx);
                view.header = false;
                view.set_layout(zoom, width, cx);
                view
            });
            // What the fork's composer asks the workspace for (an `@` path's matches) is
            // asked for it; the answer comes back through this view ([`Self::files_found`]).
            let hearing = cx.subscribe(&view, |_this, _view, event: &ThreadViewEvent, cx| {
                if let ThreadViewEvent::FindFiles { .. } = event {
                    cx.emit(event.clone());
                }
            });
            self.asides_heard = Some(hearing);
            if asked {
                view.update(cx, |v, cx| v.focus(window, cx));
            }
            if let Some(aside) = self.aside.as_mut() {
                aside.view = Some(view);
            }
        }
        let aside = self.aside.as_ref()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let head = div()
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.md))
            .py(self.z(theme.spacing.xs))
            .border_b(kit::hair(theme))
            .border_color(hsla(s.border_subtle))
            .child(self.icon(IconName::MessageCircleQuestionMark, s.text_muted))
            .child(
                div()
                    .flex_1()
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_secondary))
                    .child("Aside"),
            )
            .children(aside.thread.map(|_| {
                self.button("aside-keep", "Keep as a thread", ButtonKind::Ghost)
                    .on_click(cx.listener(|this, _ev, _w, cx| this.keep_aside(cx)))
                    .into_any_element()
            }))
            .child(self.icon_button("aside-close", IconName::X, "Close the aside").on_click(
                cx.listener(|this, _ev, _w, cx| {
                    this.close_aside(cx);
                }),
            ));
        let body = aside.view.clone().map_or_else(
            || {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from("Forking the thread\u{2026}"))
                    .into_any_element()
            },
            |view| div().flex_1().min_h_0().child(view).into_any_element(),
        );
        Some(
            kit::elevate(div(), theme)
                .id("thread-aside")
                .debug_selector(|| "thread-aside".to_owned())
                .role(Role::Dialog)
                .aria_label("Aside")
                .flex_none()
                .h(relative(SHEET))
                .mx(self.z(theme.spacing.sm))
                .flex()
                .flex_col()
                .overflow_hidden()
                .rounded(self.z(theme.radii.lg))
                .child(head)
                .child(body)
                .into_any_element(),
        )
    }
}
