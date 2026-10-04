//! ⌘F in a thread: a find bar over the list, the match on show washed and brought into view.
//!
//! The matches run newest first, as the thread reads from its end: ↵ steps back through them
//! and ⇧↵ forward, round. The items the client holds are matched as the person types
//! ([`find::matches`]); where the thread has older turns than it holds, the worker is asked
//! too (`ThreadHub::search`, after [`ASK_AFTER`] of quiet) and its hits in those turns come
//! after the held ones. Going to one pages the thread back until its turn is held. A match in
//! a folded turn, a closed group or a closed step opens what hides it.

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, Focusable as _, FollowMode,
    InteractiveElement as _, IntoElement as _, ListOffset, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use slopty_proto::thread::{ItemBody, ThreadState};

use super::ThreadView;
use crate::colors::hsla;
use crate::conversation::thread::find::{self, Found};
use crate::conversation::thread::rows::Row;
use crate::icons::IconName;
use crate::kit;

/// How long the words rest before the worker is asked for older turns.
pub(crate) const ASK_AFTER: Duration = Duration::from_millis(120);

/// The fewest characters the worker is asked to search for.
const ASK_FROM: usize = 2;

/// The find bar's width, in points.
const FIND_WIDTH: f32 = 320.0;

/// The find bar: its field, the matches newest first, the one on show, and the older turns'
/// matches the worker left out.
pub(super) struct Finder {
    field: Entity<InputState>,
    /// The words last matched: the field also says it changed when only its caret did.
    words: String,
    found: Vec<Found>,
    at: usize,
    more: u32,
    /// A match in an older turn the thread is paging back to.
    going: Option<Found>,
    /// The first turn held when the thread was last asked for older ones: it is asked again
    /// only once that page came.
    paged_from: Option<slopty_proto::thread::TurnId>,
    asking: Option<Task<()>>,
    _typing: [Subscription; 2],
}

impl ThreadView {
    /// Open the find bar, or give it the keyboard again.
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = &self.finder {
            find.field.update(cx, |f, cx| f.focus(window, cx));
            return;
        }
        let field = cx.new(|cx| InputState::new(window, cx).placeholder("Find in the thread"));
        let typing =
            cx.subscribe_in(&field, window, |this, _field, event, _window, cx| match event {
                InputEvent::Change => this.search(cx),
                InputEvent::PressEnter { shift, .. } => {
                    this.step_find(if *shift { -1 } else { 1 }, cx);
                }
                InputEvent::Focus | InputEvent::Blur => {}
            });
        let watching = cx.observe(&field, |_, _, cx| cx.notify());
        field.update(cx, |f, cx| f.focus(window, cx));
        self.finder = Some(Finder {
            field,
            words: String::new(),
            found: Vec::new(),
            at: 0,
            more: 0,
            going: None,
            paged_from: None,
            asking: None,
            _typing: [typing, watching],
        });
        cx.notify();
    }

    /// Close the find bar and give the composer the keyboard. Whether it was open.
    pub(super) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.finder.take().is_none() {
            return false;
        }
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        cx.notify();
        true
    }

    /// Whether the find bar's field has the keyboard.
    pub(super) fn find_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.finder.as_ref().is_some_and(|f| f.field.read(cx).focus_handle(cx).is_focused(window))
    }

    /// The words in the find bar.
    fn query(&self, cx: &gpui::App) -> Option<String> {
        Some(self.finder.as_ref()?.field.read(cx).value().trim().to_owned())
    }

    /// The words changed: match them again from the newest, and ask the worker for the older
    /// turns once they rest.
    fn search(&mut self, cx: &mut Context<Self>) {
        let Some(query) = self.query(cx) else { return };
        let older = self.state(cx).is_some_and(|st| st.older);
        if let Some(find) = &mut self.finder {
            if find.words == query {
                return;
            }
            find.words.clone_from(&query);
            find.asking = None;
            find.going = None;
            find.at = 0;
        }
        if older && query.chars().count() >= ASK_FROM {
            let task = cx.spawn(async move |this, cx| {
                cx.background_executor().timer(ASK_AFTER).await;
                let _gone = this.update(cx, |this, cx| {
                    this.hub.update(cx, |hub, cx| hub.search(&query, cx));
                    if let Some(find) = &mut this.finder {
                        find.asking = None;
                    }
                });
            });
            if let Some(find) = &mut self.finder {
                find.asking = Some(task);
            }
        }
        self.refind(cx);
        self.reveal(cx);
    }

    /// Match the words again over what the thread now holds and what the worker found,
    /// keeping the match on show where it still is one.
    pub(super) fn refind(&mut self, cx: &mut Context<Self>) {
        let Some(query) = self.query(cx) else { return };
        let (found, more) = match self.state(cx) {
            Some(state) => {
                let mut found = find::matches(state, &query);
                found.reverse();
                let hits = self.hub.read(cx).hits(&query);
                let (mut older, more) =
                    hits.map(|h| find::older(h, self.thread, state)).unwrap_or_default();
                older.reverse();
                found.extend(older);
                (found, more)
            }
            None => (Vec::new(), 0),
        };
        let Some(find) = &mut self.finder else { return };
        let shown = find.found.get(find.at).map(|f| f.item.clone());
        find.at = shown.and_then(|item| found.iter().position(|f| f.item == item)).unwrap_or(0);
        find.found = found;
        find.more = more;
        cx.notify();
    }

    /// The match before (`1`, back in time) or after (`-1`), round.
    fn step_find(&mut self, delta: i8, cx: &mut Context<Self>) {
        let Some(find) = &mut self.finder else { return };
        let count = find.found.len();
        if count == 0 {
            return;
        }
        find.at = if delta < 0 {
            find.at.checked_sub(1).unwrap_or_else(|| count.saturating_sub(1))
        } else if find.at.saturating_add(1) < count {
            find.at.saturating_add(1)
        } else {
            0
        };
        self.reveal(cx);
    }

    /// Bring the match on show into view: open what hides it, or page back to its turn.
    fn reveal(&mut self, cx: &mut Context<Self>) {
        let Some(found) = self.finder.as_ref().and_then(|f| f.found.get(f.at)).cloned() else {
            cx.notify();
            return;
        };
        let Some(state) = self.state(cx) else { return };
        let Some(at) = state.items.iter().position(|i| i.id == found.item) else {
            if let Some(find) = &mut self.finder {
                find.going = Some(found);
            }
            self.page_back(cx);
            return;
        };
        let step = state
            .items
            .get(at)
            .is_some_and(|i| matches!(i.body, ItemBody::Reasoning(_) | ItemBody::Tool(_)));
        let mut changed = step && self.items_open.insert(found.item);
        match find::row_of(&self.rows, &self.spans, at).and_then(|ix| self.rows.get(ix)) {
            Some(Row::Fold { turn, open: false, .. }) => changed |= self.open.insert(*turn),
            Some(Row::Group { first, open: false }) => changed |= self.groups.insert(first.clone()),
            _ => {}
        }
        if changed {
            self.rebuild(cx);
        }
        if let Some(ix) = self.find_row(cx) {
            self.list.set_follow_mode(FollowMode::Normal);
            self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(0.0) });
        }
        cx.notify();
    }

    /// The thread moved on: match again, and go on paging back to an older match until its
    /// turn is held, or there is nothing older.
    pub(super) fn chase(&mut self, cx: &mut Context<Self>) {
        if self.finder.is_none() {
            return;
        }
        self.refind(cx);
        let Some(going) = self.finder.as_ref().and_then(|f| f.going.clone()) else { return };
        let Some(state) = self.state(cx) else { return };
        if state.items.iter().any(|i| i.id == going.item) {
            if let Some(find) = &mut self.finder {
                find.going = None;
            }
            self.reveal(cx);
        } else if further_back(state, &going) {
            self.page_back(cx);
        } else if let Some(find) = &mut self.finder {
            find.going = None;
        }
    }

    /// Ask for the turns before the first held, once per page.
    fn page_back(&mut self, cx: &mut Context<Self>) {
        let first = self.state(cx).and_then(|st| st.turns.first()).map(|t| t.id);
        let Some(find) = &mut self.finder else { return };
        if find.paged_from != first {
            find.paged_from = first;
            self.older(cx);
        }
    }

    /// The row of the match on show.
    pub(super) fn find_row(&self, cx: &gpui::App) -> Option<usize> {
        let find = self.finder.as_ref()?;
        let found = find.found.get(find.at)?;
        let at = self.state(cx)?.items.iter().position(|i| i.id == found.item)?;
        find::row_of(&self.rows, &self.spans, at)
    }

    /// The match on show and how many there are, while the bar is open.
    #[cfg(test)]
    pub(crate) fn found(&self) -> Option<(usize, usize)> {
        self.finder.as_ref().map(|f| (f.at, f.found.len()))
    }

    /// The find bar, at the list's top right while it is open.
    pub(super) fn find_bar(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let find = self.finder.as_ref()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let count = find.found.len();
        let typed = !find.field.read(cx).value().trim().is_empty();
        let tally = match (count, typed) {
            (0, false) => String::new(),
            (0, true) if find.asking.is_some() || find.going.is_some() => "Finding…".to_owned(),
            (0, true) => "No matches".to_owned(),
            _ if find.more > 0 => format!("{} of {count}+", find.at.saturating_add(1)),
            _ => format!("{} of {count}", find.at.saturating_add(1)),
        };
        let step = |id: &'static str, icon: IconName, label: &'static str, delta: i8| {
            self.icon_button(id, icon, label)
                .on_click(cx.listener(move |this, _ev, _w, cx| this.step_find(delta, cx)))
        };
        let close = self.icon_button("thread-find-close", IconName::X, "Close find").on_click(
            cx.listener(|this, _ev, window, cx| {
                this.close_find(window, cx);
            }),
        );
        let bar = kit::elevate(div(), theme)
            .id("thread-find")
            .debug_selector(|| "thread-find".to_owned())
            .role(Role::Search)
            .aria_label("Find in the thread")
            .w(self.z(FIND_WIDTH))
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .pl(self.z(theme.spacing.sm))
            .pr(self.z(theme.spacing.xxs))
            .py(self.z(theme.spacing.xxs))
            .rounded(self.z(theme.radii.lg))
            .text_size(self.z(theme.typography.small()))
            .child(self.icon(IconName::Search, s.text_muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&find.field).appearance(false).aria_label("Find")),
            )
            .child(
                kit::tabular(div())
                    .debug_selector(|| "thread-find-count".to_owned())
                    .flex_none()
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(tally)),
            )
            .child(step("thread-find-newer", IconName::ChevronDown, "Newer match", -1))
            .child(step("thread-find-older", IconName::ChevronUp, "Older match", 1))
            .child(close);
        Some(
            div()
                .absolute()
                .top(self.z(theme.spacing.sm))
                .right(self.z(theme.spacing.lg))
                .child(kit::slide_fade(
                    bar,
                    "thread-find",
                    -theme.spacing.xs * self.zoom,
                    kit::Pace::Fade,
                    cx,
                ))
                .into_any_element(),
        )
    }
}

/// Whether paging back can still reach `going`: the thread has older turns and holds none
/// as old as its turn.
fn further_back(state: &ThreadState, going: &Found) -> bool {
    state.older && state.turns.first().is_some_and(|t| t.id > going.turn)
}
