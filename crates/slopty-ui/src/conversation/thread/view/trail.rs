//! A subagent's thread, opened in the same view from the call that started it: a bar over it
//! names the subagent and leads back, Esc too. The thread above stays followed meanwhile, so
//! going back draws it at once; the subagent's is let go on the way back.

use std::collections::HashSet;

use gpui::accesskit::Role;
use gpui::{
    AnyElement, App, Context, FontWeight, InteractiveElement as _, IntoElement as _, ListOffset,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};
use slopty_proto::thread::{ItemId, ThreadId, TurnId};
use slopty_theme::Typography;

use super::ThreadView;
use crate::colors::hsla;
use crate::icons::Symbol;
use crate::kit;

/// A thread above the subagent's on show, as the reader left it.
#[derive(Debug)]
pub(super) struct Above {
    thread: ThreadId,
    open: HashSet<TurnId>,
    shut: HashSet<TurnId>,
    kept: HashSet<TurnId>,
    items_open: HashSet<ItemId>,
    whole: HashSet<ItemId>,
    scroll: ListOffset,
    /// What the call that opened the thread below calls the subagent.
    called: String,
}

impl Above {
    /// The thread.
    pub(super) const fn thread(&self) -> ThreadId {
        self.thread
    }
}

impl ThreadView {
    /// Whether a subagent's thread is on show rather than the tile's own.
    pub(super) const fn in_subagent(&self) -> bool {
        !self.trail.is_empty()
    }

    /// The title of `thread`, as its state or the table says it.
    fn title_of(&self, thread: ThreadId, cx: &App) -> Option<String> {
        self.hub.read(cx).threads().title(thread).map(str::to_owned)
    }

    /// Open subagent thread `child` in the view, the one on show kept as the reader left it
    /// to go back to. The view itself takes the keyboard: a subagent has no composer.
    pub(crate) fn open_subagent(
        &mut self,
        child: ThreadId,
        called: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if child == self.thread {
            return;
        }
        self.hub.update(cx, |hub, cx| hub.open(child, cx));
        self.trail.push(Above {
            thread: self.thread,
            open: std::mem::take(&mut self.open),
            shut: std::mem::take(&mut self.shut),
            kept: std::mem::take(&mut self.kept),
            items_open: std::mem::take(&mut self.items_open),
            whole: std::mem::take(&mut self.whole),
            scroll: self.list.logical_scroll_top(),
            called,
        });
        self.thread = child;
        self.asked_at = 0;
        self.plan_open = false;
        self.rebuild(cx);
        self.list.scroll_to_end();
        window.focus(&self.focus, cx);
    }

    /// Back to the thread above, as the reader left it; the composer has the keyboard again
    /// once the tile's own thread is back. Whether a subagent's was on show.
    pub(super) fn leave_subagent(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(above) = self.trail.pop() else { return false };
        let left = self.thread;
        self.hub.update(cx, |hub, cx| hub.close(left, cx));
        self.thread = above.thread;
        self.open = above.open;
        self.shut = above.shut;
        self.kept = above.kept;
        self.items_open = above.items_open;
        self.whole = above.whole;
        self.asked_at = 0;
        self.rebuild(cx);
        self.list.scroll_to(above.scroll);
        if self.trail.is_empty() {
            self.composer.update(cx, |c, cx| c.focus(window, cx));
        }
        true
    }

    /// The bar over a subagent's thread: the way back and its name.
    pub(super) fn trail_bar(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !self.in_subagent() {
            return None;
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        // The thread's own title when it has one, else what its call called it.
        let title = self
            .title_of(self.thread, cx)
            .or_else(|| self.trail.last().map(|above| above.called.clone()))
            .unwrap_or_default();
        let label = SharedString::from(format!("Subagent {title}"));
        Some(
            div()
                .id("thread-trail")
                .debug_selector(|| "thread-trail".to_owned())
                .role(Role::Navigation)
                .aria_label(label)
                .flex_none()
                .w_full()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .px(px(theme.spacing.sm))
                .min_h(px(kit::Row::One.height(theme)))
                .child(self.icon_button("thread-back", Symbol::ChevronLeft, "Back").on_click(
                    cx.listener(|this, _ev, window, cx| {
                        let _was = this.leave_subagent(window, cx);
                    }),
                ))
                .child(self.icon(crate::icons::AGENT, s.text_muted))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(theme.typography.small()))
                        .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                        .text_color(hsla(s.text))
                        .child(SharedString::from(title)),
                )
                .into_any_element(),
        )
    }
}
