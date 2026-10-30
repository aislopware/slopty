//! slopty: find in the current thread (⌘F). A field on the transcript's own overlay card,
//! under the titlebar: the tally ("2 of 5"), ↑ and ↓ to step, every match washed where it
//! stands and the current one stronger, landed below the titlebar the way every jump lands.
//! It looks through what the thread says: the person's prompts and the answers' text, plans
//! included, in the order they read. The keys are the shell keymap's ([`FindInThread`],
//! [`FindNext`], [`FindPrevious`]); with no binding of the shell's in the way the actions reach
//! the window's main transcript through the app.

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable as _, Global, SharedString,
    Subscription, WeakEntity, Window, div, prelude::*, px,
};

use super::{Row, RowKind, Transcript, fnv1a};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::markdown::find as paint;
use crate::popover;
use crate::settings::widgets::{ActionHint as _, Keys};
use crate::theme::Theme;

gpui::actions!(
    transcript,
    [
        /// Open find in the current thread, or give its field the keyboard.
        FindInThread,
        /// Step to the next match.
        FindNext,
        /// Step to the previous match.
        FindPrevious
    ]
);

/// The open find of one transcript.
#[derive(Default)]
pub(crate) struct FindState {
    pub(super) open: bool,
    input: Option<Entity<ComposerInput>>,
    _events: Option<Subscription>,
    /// The words the hits were counted for.
    query: String,
    /// Each row with matches: its index, its id, its count.
    hits: Vec<(usize, SharedString, usize)>,
    total: usize,
    /// The current match, by its place among all of them.
    active: Option<usize>,
    /// The rows the hits were counted over.
    signature: u64,
    /// Where the keyboard was before the field took it.
    return_focus: Option<FocusHandle>,
}

impl FindState {
    /// "2 of 5", "No results", or nothing before a word is typed.
    pub(crate) fn tally(&self) -> Option<String> {
        if self.query.is_empty() {
            return None;
        }
        Some(match (self.active, self.total) {
            (_, 0) => "No results".to_owned(),
            (Some(at), total) => format!("{} of {total}", at + 1),
            (None, total) => format!("{total} found"),
        })
    }

    /// The row and the place within it of the match at `at`.
    fn locate(&self, at: usize) -> Option<(usize, SharedString, usize)> {
        let mut seen = 0;
        for (ix, id, count) in &self.hits {
            if at < seen + count {
                return Some((*ix, id.clone(), at - seen));
            }
            seen += count;
        }
        None
    }
}

/// The texts a row paints, one per text element, in paint order (what find looks through).
pub(crate) fn row_texts(row: &Row) -> Vec<String> {
    match &row.kind {
        RowKind::User { text, .. } => vec![text.to_string()],
        RowKind::Markdown { tree, block_ix } | RowKind::LiveMarkdown { tree, block_ix } => {
            let mut out = Vec::new();
            if let Some(top) = tree.blocks.get(*block_ix) {
                paint::block_texts(&top.block, &mut out);
            }
            out
        }
        _ => Vec::new(),
    }
}

/// Each row with matches of `needle`: its index, its id and how many it holds.
pub(crate) fn hits(rows: &[Row], needle: &str) -> Vec<(usize, SharedString, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    rows.iter()
        .enumerate()
        .filter_map(|(ix, row)| {
            let count = paint::count(&row_texts(row), needle);
            (count > 0).then(|| (ix, row.id.clone(), count))
        })
        .collect()
}

fn rows_signature(rows: &[Row]) -> u64 {
    let mut key = Vec::with_capacity(rows.len() * 24);
    for row in rows {
        key.extend_from_slice(row.id.as_bytes());
        key.extend_from_slice(&row.version.to_le_bytes());
    }
    fnv1a(&key)
}

/// The window's main transcript, for find's keys pressed where no view takes them, and the
/// window it draws in.
struct FindTarget(WeakEntity<Transcript>, Option<gpui::AnyWindowHandle>);

impl Global for FindTarget {}

/// The app's find actions are registered.
struct FindActions;

impl Global for FindActions {}

fn with_target(cx: &mut App, act: impl FnOnce(&mut Transcript, &mut Window, &mut Context<Transcript>) + 'static) {
    let Some((target, window)) = cx
        .try_global::<FindTarget>()
        .and_then(|target| Some((target.0.upgrade()?, target.1)))
    else {
        return;
    };
    let Some(window) = window.or_else(|| cx.active_window()) else {
        return;
    };
    // slopty: a key the window dispatches reaches this while the window is being updated, which
    // cannot be updated again inside itself: the transcript acts once the dispatch is done.
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, window, cx| {
            target.update(cx, |transcript, cx| act(transcript, window, cx));
        });
    });
}

impl Transcript {
    /// Make this the transcript find's keys reach, and register them once.
    pub(super) fn install_find(cx: &mut Context<Self>) {
        cx.set_global(FindTarget(cx.weak_entity(), None));
        if cx.has_global::<FindActions>() {
            return;
        }
        cx.set_global(FindActions);
        let app: &mut App = cx;
        app.on_action(|_: &FindInThread, cx| with_target(cx, Self::open_find));
        app.on_action(|_: &FindNext, cx| {
            with_target(cx, |this, window, cx| this.find_step(1, window, cx))
        });
        app.on_action(|_: &FindPrevious, cx| {
            with_target(cx, |this, window, cx| this.find_step(-1, window, cx))
        });
    }

    /// Whether find is open, and its tally.
    pub(crate) fn find_tally(&self) -> Option<String> {
        self.find.open.then(|| self.find.tally()).flatten()
    }

    /// Open find with the selection's words (when there is one) and give it the keyboard.
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.input.is_none() {
            let input = cx.new(|cx| {
                ComposerInput::new("Find in session", cx)
                    .with_single_line()
                    .with_accessibility_role(gpui::Role::SearchInput)
                    .with_text_metrics(12.5, 18.0)
            });
            let events = cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                ComposerInputEvent::Edited => {
                    this.find.active = None;
                    this.refresh_find(cx);
                    if this.find.total > 0 {
                        this.find_step(1, window, cx);
                    }
                    cx.notify();
                }
                ComposerInputEvent::Submitted | ComposerInputEvent::MentionAccept => {
                    this.find_step(1, window, cx)
                }
                ComposerInputEvent::MentionNavigate(delta) => this.find_step(*delta, window, cx),
                ComposerInputEvent::MentionDismiss => this.close_find(window, cx),
                _ => {}
            });
            self.find.input = Some(input);
            self.find._events = Some(events);
        }
        let Some(input) = self.find.input.clone() else {
            return;
        };
        if !self.find.open {
            self.find.return_focus = window.focused(cx);
            self.find.open = true;
            if let Some(selected) = crate::markdown::selection::selected_text()
                .map(|text| slopty_view_proto::view::single_line(&text))
                .filter(|text| !text.is_empty() && text.chars().count() <= 200)
            {
                input.update(cx, |input, cx| input.set_text(selected, cx));
            }
        }
        input.update(cx, |input, cx| input.select_all_text(cx));
        window.focus(&input.focus_handle(cx), cx);
        self.refresh_find(cx);
        cx.notify();
    }

    /// Close find, take its washes away and give the keyboard back.
    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        self.find.open = false;
        self.find.active = None;
        paint::clear(cx.entity_id().as_u64());
        if let Some(focus) = self.find.return_focus.take() {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// Step `delta` matches on (wrapping), and land the match's row below the titlebar.
    pub(crate) fn find_step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.find.open {
            self.open_find(window, cx);
            return;
        }
        self.refresh_find(cx);
        let total = self.find.total;
        if total == 0 {
            return;
        }
        let next = match self.find.active {
            None if delta < 0 => total - 1,
            None => 0,
            Some(at) => (at as isize + delta).rem_euclid(total as isize) as usize,
        };
        self.find.active = Some(next);
        self.publish_find(cx);
        if let Some((ix, _, _)) = self.find.locate(next) {
            self.glide_to_row(ix, false, cx); // slopty: its match marks the landing.
        }
        cx.notify();
    }

    /// Count the matches again when the words or the rows changed.
    fn refresh_find(&mut self, cx: &mut Context<Self>) {
        let query = self
            .find
            .input
            .as_ref()
            .map(|input| input.read(cx).text().to_owned())
            .unwrap_or_default();
        let signature = rows_signature(&self.rows);
        if query == self.find.query && signature == self.find.signature {
            return;
        }
        let kept = self
            .find
            .active
            .filter(|_| query == self.find.query)
            .and_then(|at| self.find.locate(at))
            .map(|(_, id, place)| (id, place));
        self.find.hits = hits(&self.rows, &query);
        self.find.total = self.find.hits.iter().map(|(_, _, count)| count).sum();
        self.find.query = query;
        self.find.signature = signature;
        // The current match stays the same one while rows come and go around it.
        self.find.active = kept.and_then(|(id, place)| {
            let mut seen = 0;
            for (_, row, count) in &self.find.hits {
                if *row == id {
                    return Some(seen + place.min(count.saturating_sub(1)));
                }
                seen += count;
            }
            None
        });
        self.publish_find(cx);
    }

    /// Tell the rows what to wash.
    fn publish_find(&self, cx: &mut Context<Self>) {
        let surface = cx.entity_id().as_u64();
        if !self.find.open || self.find.query.is_empty() {
            paint::clear(surface);
            return;
        }
        let active = self
            .find
            .active
            .and_then(|at| self.find.locate(at))
            .map(|(_, id, place)| (id.to_string(), place));
        paint::set(
            surface,
            paint::FindPaint {
                needle: self.find.query.clone(),
                rows: self.find.hits.iter().map(|(_, id, _)| id.to_string()).collect(),
                active,
                washes: paint::washes(Theme::of(cx)),
            },
        );
    }

    /// Whether find is open, its card over the transcript's top right.
    pub(super) fn finding(&self) -> bool {
        self.find.open
    }

    /// The find card over the transcript's top right, while find is open.
    pub(super) fn render_find_bar(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.note_find_window(window, cx);
        if !self.find.open {
            return None;
        }
        self.refresh_find(cx);
        let input = self.find.input.clone()?;
        let theme = Theme::of(cx).clone();
        let tally = self.find.tally();
        let has = self.find.total > 0;
        // slopty: at the top of the pane the transcript lays out in, which clears the window's
        // chrome itself.
        let top = Theme::SPACE_SM;
        // slopty: the house's find card (the diff's and the terminal's anatomy): its steps name
        // their keys, a step with nowhere to go dims, and it enters as a menu does.
        let card = popover::find_card(&theme)
            .id("find-in-thread")
            .role(gpui::Role::Search)
            .aria_label("Find in session")
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close_find(window, cx);
                }
            }))
            .child(div().flex_1().min_w_0().child(input))
            .children(tally.map(|tally| popover::find_tally(&theme, tally)))
            .child(
                popover::find_step("find-previous", crate::icons::ALT_ARROW_UP, has, &theme)
                    .action_hint("Previous match", Keys::Action(Box::new(FindPrevious)), cx)
                    .when(has, |el| {
                        el.on_click(cx.listener(|this, _, window, cx| this.find_step(-1, window, cx)))
                    }),
            )
            .child(
                popover::find_step("find-next", crate::icons::ALT_ARROW_DOWN, has, &theme)
                    .action_hint("Next match", Keys::Action(Box::new(FindNext)), cx)
                    .when(has, |el| {
                        el.on_click(cx.listener(|this, _, window, cx| this.find_step(1, window, cx)))
                    }),
            )
            .child(
                popover::find_step("find-close", crate::icons::CLOSE, true, &theme)
                    .action_hint("Close find", Keys::Fixed("escape"), cx)
                    .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            );
        Some(
            div()
                .absolute()
                .top(px(top))
                .right(px(Theme::SPACE_LG + self.right_reserve))
                .child(popover::find_card_in("find-in-thread-in", card))
                .into_any_element(),
        )
    }

    /// slopty: find's keys go to the window this transcript draws in, whether or not that
    /// window is the key one (`cx.active_window()` is `None` then, and ⌘F did nothing).
    fn note_find_window(&self, window: &Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        let me = cx.entity_id();
        let stale = cx.try_global::<FindTarget>().is_some_and(|target| {
            target.0.entity_id() == me && target.1 != Some(handle)
        });
        if stale {
            cx.global_mut::<FindTarget>().1 = Some(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn markdown(id: &str, text: &str) -> Row {
        Row {
            id: id.to_owned().into(),
            version: 0,
            turn_start: false,
            kind: RowKind::Markdown {
                tree: Arc::new(crate::markdown::parser::parse_full(text)),
                block_ix: 0,
            },
            entry_id: "e".into(),
            timestamp: None,
            copy_text: None,
            compact_fold: None,
        }
    }

    #[test]
    fn find_counts_and_steps_matches() {
        let rows = vec![
            markdown("a", "The cat sat."),
            markdown("b", "No match here."),
            markdown("c", "Cat and CAT, again a cat."),
        ];
        let hits = hits(&rows, "cat");
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].0, hits[0].2), (0, 1));
        assert_eq!((hits[1].0, hits[1].2), (2, 3));
        let mut state = FindState {
            open: true,
            query: "cat".into(),
            total: 4,
            hits,
            ..Default::default()
        };
        assert_eq!(state.tally().as_deref(), Some("4 found"));
        state.active = Some(1);
        assert_eq!(state.tally().as_deref(), Some("2 of 4"));
        let (ix, id, place) = state.locate(3).unwrap();
        assert_eq!((ix, id.as_ref(), place), (2, "c", 2));
        assert_eq!(state.locate(4), None);
        state.total = 0;
        assert_eq!(state.tally().as_deref(), Some("No results"));
    }
    /// ⌘F reaches the window's transcript from a key the window dispatches, the keyboard
    /// elsewhere and the window not the key one: find opens and takes the keyboard.
    #[gpui::test]
    fn find_opens_from_the_windows_keys_key_or_not(cx: &mut gpui::TestAppContext) {
        use gpui::prelude::*;
        struct Fixture(Entity<Transcript>, gpui::FocusHandle);
        impl gpui::Render for Fixture {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                gpui::div().size_full().track_focus(&self.1).child(self.0.clone())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            cx.set_reduce_motion(true);
        });
        let state = cx.new(|_| crate::state::AppState::new());
        let transcript = cx.new(|cx| Transcript::new(state, cx));
        let (fixture, cx) =
            cx.add_window_view(|_, cx| Fixture(transcript.clone(), cx.focus_handle()));
        let elsewhere = fixture.read_with(cx, |fixture, _| fixture.1.clone());
        cx.update(|window, cx| window.focus(&elsewhere, cx));
        cx.run_until_parked();
        cx.deactivate_window();
        cx.update(|window, cx| window.dispatch_action(Box::new(FindInThread), cx));
        cx.run_until_parked();
        assert!(transcript.read_with(cx, |this, _| this.finding()), "find opened");
        let keyboard = cx.update(|window, cx| {
            transcript.read(cx).find.input.as_ref().is_some_and(|input| input.focus_handle(cx).is_focused(window))
        });
        assert!(keyboard, "and has the keyboard");
    }
}
