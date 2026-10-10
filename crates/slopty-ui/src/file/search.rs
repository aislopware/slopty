//! A file tile's find bar, and the replace under it.
//!
//! ⌘F opens it over the tile's top-right corner, as the terminal's; ⌘⇧H opens it with the
//! replace field. A match is a byte range of the text ([`super::find`]): tinted in the
//! terminal's search colours, the one the tile is on in the stronger, stepped with ⌘G/↩ and
//! wrapped. The query takes text or a regular expression, whole words, and smart or exact
//! case, with the search surface's toggles and keys. ↩ in the replace field replaces the match
//! the tile is on and goes to the next; ⌘↩ replaces them all, as one undo step.

use std::ops::Range;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_kit::component::input::{InputEvent, InputState, RangeDecoration, RangeDecorationStyle};
use regex::Regex;

use super::find::{self, MATCHES_MAX};
use super::{FileView, FileViewEvent};
use crate::colors::hsla;
use crate::kit::FindBar;
use crate::kit::find::{
    PLACEHOLDER as FIND_PLACEHOLDER, Query, REPLACE_PLACEHOLDER, Tally, Toggle,
};
use crate::search::{ToggleMatchCase, ToggleRegex, ToggleWholeWord};
use crate::terminal::{CloseFind, FindNext, FindPrev};

/// The key context of the find bar; its keys are bound in it.
pub const SEARCH_CTX: &str = "FileSearch";

/// The open find bar of a file tile.
pub(super) struct FileSearch {
    input: Entity<InputState>,
    /// The replace field, while replacing.
    replace: Option<Entity<InputState>>,
    /// What is asked.
    query: Query,
    /// The query compiled; none for an empty needle or a pattern that does not compile.
    matcher: Option<Regex>,
    /// Why the pattern does not compile.
    error: Option<String>,
    /// The matches, in order, at most [`MATCHES_MAX`].
    matches: Vec<Range<usize>>,
    /// Index into `matches` of the one the tile is on.
    current: Option<usize>,
    /// Where a find again lands from: the caret when the bar opened, then the match stepped to
    /// or the end of the text a replace put in. Typing a longer needle stays on its match.
    anchor: usize,
    subscriptions: Vec<Subscription>,
}

impl FileView {
    /// ⌘F: open the find bar, or put the caret back in it with the text selected.
    pub fn find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The hits are marked in the source.
        if self.previewing() {
            self.show_preview(false, window, cx);
        }
        // One field over the corner at a time: the find bar takes the place of the others.
        self.goto = None;
        if self.search.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(FIND_PLACEHOLDER));
            let subscription = cx.subscribe(&input, |this, _input, event, cx| match event {
                InputEvent::Change => this.search_changed(cx),
                InputEvent::PressEnter { shift, .. } => {
                    this.step_hit(if *shift { -1 } else { 1 }, cx);
                }
                InputEvent::Focus | InputEvent::Blur => {}
            });
            self.search = Some(FileSearch {
                input,
                replace: None,
                query: Query::default(),
                matcher: None,
                error: None,
                matches: Vec::new(),
                current: None,
                anchor: self.editor.read(cx).selected_range().start,
                subscriptions: vec![subscription],
            });
        }
        if let Some(search) = &self.search {
            search.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
        cx.notify();
    }

    /// ⌘⇧H: the find bar with its replace field, the keyboard in the replace field once there
    /// is something to find. A second ⌘⇧H closes the replace field.
    pub fn toggle_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        if self.search.as_ref().is_some_and(|s| s.replace.is_some()) {
            if let Some(search) = &mut self.search {
                search.replace = None;
                search.subscriptions.truncate(1);
                search.input.update(cx, |input, cx| input.focus(window, cx));
            }
            cx.notify();
            return;
        }
        self.find(window, cx);
        let field = cx.new(|cx| InputState::new(window, cx).placeholder(REPLACE_PLACEHOLDER));
        let subscription = cx.subscribe_in(&field, window, |this, _field, event, window, cx| {
            if let InputEvent::PressEnter { secondary, .. } = event {
                if *secondary {
                    this.replace_all(window, cx);
                } else {
                    this.replace_one(window, cx);
                }
            }
        });
        let Some(search) = &mut self.search else { return };
        if !search.query.needle.is_empty() {
            field.update(cx, |f, cx| f.focus(window, cx));
        }
        search.replace = Some(field);
        search.subscriptions.push(subscription);
        cx.notify();
    }

    /// Whether the replace field is open.
    #[must_use]
    pub fn replacing(&self) -> bool {
        self.search.as_ref().is_some_and(|s| s.replace.is_some())
    }

    /// Open the find bar on `query` (a search of the open tiles chose this one): the field
    /// holds its needle, the toggles are its own, and the tile lands on the first match.
    pub fn find_with(&mut self, query: &Query, window: &mut Window, cx: &mut Context<Self>) {
        self.find(window, cx);
        let Some(search) = &mut self.search else { return };
        search.input.update(cx, |input, cx| input.set_value(query.needle.clone(), window, cx));
        search.query = query.clone();
        self.recompile(cx);
    }

    /// Esc or ✕ in the find bar: close it; the editor takes the keyboard back.
    pub fn close_find(&mut self, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            self.remark(cx);
            cx.emit(FileViewEvent::FindClosed);
            cx.notify();
        }
    }

    /// Whether the find bar is open.
    #[must_use]
    pub const fn finding(&self) -> bool {
        self.search.is_some()
    }

    /// The matches found (byte ranges) and which one the tile is on.
    #[must_use]
    pub fn hits(&self) -> Option<(&[Range<usize>], Option<usize>)> {
        self.search.as_ref().map(|s| (s.matches.as_slice(), s.current))
    }

    /// The lines (0-based) holding a match, each once.
    #[must_use]
    pub fn hit_rows(&self, cx: &gpui::App) -> Vec<usize> {
        let Some(search) = &self.search else { return Vec::new() };
        find::rows(&self.text(cx), &search.matches)
    }

    /// Why the pattern does not compile, while it does not.
    #[must_use]
    pub fn find_error(&self) -> Option<&str> {
        self.search.as_ref().and_then(|s| s.error.as_deref())
    }

    /// Flip one of the query's toggles (case, whole word, pattern) and find again.
    pub fn toggle_query(&mut self, toggle: Toggle, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        search.query.flip(toggle);
        self.recompile(cx);
    }

    /// The query's toggles, for a test or the dump: case, whole word, pattern.
    #[must_use]
    pub fn query(&self) -> Option<&Query> {
        self.search.as_ref().map(|s| &s.query)
    }

    fn search_changed(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        let needle = search.input.read(cx).value().to_string();
        if needle == search.query.needle {
            return;
        }
        search.query.needle = needle;
        self.recompile(cx);
    }

    /// Compile the query again (it changed) and find.
    fn recompile(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        match search.query.matcher() {
            Ok(matcher) => {
                search.matcher = matcher;
                search.error = None;
            }
            Err(error) => {
                search.matcher = None;
                search.error = Some(error);
            }
        }
        self.refresh_hits(cx);
    }

    /// Find again (the query or the text changed) and land on the first match at or after the
    /// caret.
    pub(super) fn refresh_hits(&mut self, cx: &mut Context<Self>) {
        let Some(matcher) = self.search.as_ref().map(|s| s.matcher.clone()) else { return };
        let matches = matcher.map(|m| find::matches(&self.text(cx), &m)).unwrap_or_default();
        let Some(search) = &mut self.search else { return };
        let from = search.anchor;
        search.matches = matches;
        search.current = if search.matches.is_empty() {
            None
        } else {
            Some(search.matches.iter().position(|m| m.start >= from).unwrap_or(0))
        };
        self.go_to_hit(cx);
        self.remark(cx);
        cx.notify();
    }

    /// ⌘G / ↩ (+1) and ⌘⇧G / ⇧↩ (−1), wrapping.
    pub fn step_hit(&mut self, delta: i64, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        let count = i64::try_from(search.matches.len()).unwrap_or(0);
        if count == 0 {
            return;
        }
        let at = i64::try_from(search.current.unwrap_or(0)).unwrap_or(0);
        search.current = usize::try_from(at.saturating_add(delta).rem_euclid(count)).ok();
        if let Some(m) = search.current.and_then(|c| search.matches.get(c)) {
            search.anchor = m.start;
        }
        self.go_to_hit(cx);
        self.remark(cx);
        cx.notify();
    }

    /// Select the current match, which scrolls it into view.
    fn go_to_hit(&self, cx: &mut Context<Self>) {
        let Some(search) = &self.search else { return };
        let Some(range) = search.current.and_then(|c| search.matches.get(c)).cloned() else {
            return;
        };
        self.editor.update(cx, |e, cx| {
            let end = e.text().len();
            e.set_selected_range(range.start.min(end)..range.end.min(end), cx);
        });
    }

    /// The replace field's text, and whether its groups expand (a pattern's do).
    fn replacement(&self, cx: &gpui::App) -> Option<(String, bool)> {
        let search = self.search.as_ref()?;
        let field = search.replace.as_ref()?;
        Some((field.read(cx).value().to_string(), search.query.regex))
    }

    /// ↩ in the replace field: the match the tile is on replaced, then on to the next.
    pub fn replace_one(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let Some((with, expand)) = self.replacement(cx) else { return };
        let Some(search) = &self.search else { return };
        let (Some(matcher), Some(at)) =
            (search.matcher.clone(), search.current.and_then(|c| search.matches.get(c)).cloned())
        else {
            return;
        };
        let text = self.text(cx);
        let Some(new) = find::replacement(&text, &matcher, &at, &with, expand) else {
            self.refresh_hits(cx);
            return;
        };
        let after = at.start.saturating_add(new.len());
        self.editor.update(cx, |e, cx| {
            e.set_selected_range(at.clone(), cx);
            e.replace(new, window, cx);
            e.set_selected_range(after..after, cx);
        });
        // The edit finds again from past the replaced text: the next match.
        if let Some(search) = &mut self.search {
            search.anchor = after;
        }
        self.edited(cx);
    }

    /// ⌘↩ in the replace field: every match replaced, as one edit and one undo step.
    pub fn replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        let Some((with, expand)) = self.replacement(cx) else { return };
        let Some(matcher) = self.search.as_ref().and_then(|s| s.matcher.clone()) else { return };
        let text = self.text(cx);
        let Some((span, new, count)) = find::replace_all(&text, &matcher, &with, expand) else {
            return;
        };
        tracing::debug!(path = %self.path, count, "replace all");
        let caret = self.editor.read(cx).selected_range().start;
        let grown = new.len().cast_signed().saturating_sub(span.len().cast_signed());
        let caret = if caret >= span.end {
            caret.saturating_add_signed(grown)
        } else {
            caret.min(span.start)
        };
        self.editor.update(cx, |e, cx| {
            e.set_selected_range(span, cx);
            e.replace(new, window, cx);
            e.set_selected_range(caret..caret, cx);
        });
        self.edited(cx);
    }

    /// The tints of the matches: the terminal's search colours, the current one stronger.
    pub(super) fn search_marks(&self) -> Vec<RangeDecoration> {
        let Some(search) = &self.search else { return Vec::new() };
        let palette = &self.theme.terminal;
        let (all, current) = (hsla(palette.search_match), hsla(palette.search_current));
        search
            .matches
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.is_empty())
            .map(|(ix, m)| {
                RangeDecoration::new(m.clone())
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(if Some(ix) == search.current { current } else { all })
            })
            .collect()
    }

    /// The tile's handlers for the find bar's keys.
    pub(super) fn search_keys(
        el: gpui::Stateful<gpui::Div>,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        el.on_action(cx.listener(|this, _: &CloseFind, _window, cx| this.close_find(cx)))
            .on_action(cx.listener(|this, _: &FindNext, _window, cx| this.step_hit(1, cx)))
            .on_action(cx.listener(|this, _: &FindPrev, _window, cx| this.step_hit(-1, cx)))
            .on_action(cx.listener(|this, _: &ToggleMatchCase, _window, cx| {
                this.toggle_query(Toggle::MatchCase, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleWholeWord, _window, cx| {
                this.toggle_query(Toggle::WholeWord, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRegex, _window, cx| {
                this.toggle_query(Toggle::Regex, cx);
            }))
    }

    /// The find bar over the top-right corner, as the terminal's, with the replace row under
    /// it while replacing.
    pub(super) fn render_search(&self, search: &FileSearch, cx: &Context<Self>) -> AnyElement {
        let tally = if search.query.needle.is_empty() {
            Tally::Quiet
        } else if let Some(why) = &search.error {
            Tally::Bad(why.clone())
        } else {
            Tally::Found {
                at: search.current,
                total: search.matches.len(),
                more: search.matches.len() >= MATCHES_MAX,
            }
        };
        let this = cx.entity().downgrade();
        let (stepping, closing, replacing) = (this.clone(), this.clone(), this.clone());
        let bar = FindBar::new("file-find", "Find in file", &search.input, tally, &self.theme)
            .toggles(&search.query, move |toggle, _window, cx| {
                let _gone = this.update(cx, |v, cx| v.toggle_query(toggle, cx));
            })
            .on_step(move |delta, _window, cx| {
                let _gone = stepping.update(cx, |v, cx| v.step_hit(i64::from(delta), cx));
            })
            .on_close(move |_window, cx| {
                let _gone = closing.update(cx, Self::close_find);
            })
            .when_some(search.replace.as_ref(), |bar, field| {
                bar.replace(field, move |all, window, cx| {
                    let _gone = replacing.update(cx, |v, cx| {
                        if all {
                            v.replace_all(window, cx);
                        } else {
                            v.replace_one(window, cx);
                        }
                    });
                })
            });
        let spacing = self.theme.spacing;
        div()
            .key_context(SEARCH_CTX)
            .absolute()
            .top(px(spacing.sm))
            .right(px(spacing.sm))
            .id("file-find-keys")
            .map(|el| Self::search_keys(el, cx))
            .child(bar)
            .into_any_element()
    }
}

impl FileSearch {
    /// The replace field, while replacing.
    #[cfg(test)]
    pub(super) const fn replace_field(&self) -> Option<&Entity<InputState>> {
        self.replace.as_ref()
    }
}
