//! A file tile's find bar, and the replace under it.
//!
//! ⌘F opens it over the tile's top-right corner, as the terminal's; ⌘⇧H opens it with the
//! replace field. A match is a byte range of the text ([`super::find`]): tinted in the
//! terminal's search colours, the one the tile is on in the stronger, stepped with ⌘G/↩ and
//! wrapped. The query takes text or a regular expression, whole words, and smart or exact
//! case, with the search surface's toggles and keys. ↩ in the replace field replaces the match
//! the tile is on and goes to the next; ⌘↩ replaces them all, as one undo step.

use std::ops::Range;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement as _,
    MouseButton, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, px,
};
use gpui_kit::component::input::{
    Input, InputEvent, InputState, RangeDecoration, RangeDecorationStyle,
};
use regex::Regex;
use slopty_theme::alpha;

use super::find::{self, MATCHES_MAX, Query};
use super::{FileView, FileViewEvent};
use crate::colors::{hsla, hsla_alpha};
use crate::icons::IconName;
use crate::kit::FIND_PLACEHOLDER;
use crate::search::{
    MATCH_CASE, REGEX, REPLACE_ALL, REPLACE_LINE, REPLACE_PLACEHOLDER, ToggleMatchCase,
    ToggleRegex, ToggleWholeWord, WHOLE_WORD,
};
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
        // One field over the corner at a time: the find bar takes the place of the others.
        self.goto = None;
        self.symbols = None;
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

    /// Open the find bar on `needle` (a find in every tile chose this one): the field holds
    /// it and the tile lands on the first match.
    pub fn find_with(&mut self, needle: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.find(window, cx);
        let Some(search) = &mut self.search else { return };
        search.input.update(cx, |input, cx| input.set_value(needle.to_owned(), window, cx));
        needle.clone_into(&mut search.query.needle);
        self.recompile(cx);
    }

    /// The find bar's needle, when the bar is open.
    #[must_use]
    pub fn search_needle(&self) -> Option<&str> {
        self.search.as_ref().map(|s| s.query.needle.as_str())
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
    pub fn toggle_query(&mut self, which: fn(&mut Query) -> &mut bool, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        let on = which(&mut search.query);
        *on = !*on;
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
                this.toggle_query(|q| &mut q.match_case, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleWholeWord, _window, cx| {
                this.toggle_query(|q| &mut q.whole_word, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRegex, _window, cx| {
                this.toggle_query(|q| &mut q.regex, cx);
            }))
    }

    /// The find bar over the top-right corner, as the terminal's: the field, the query's
    /// toggles, the count, the steps and the close; under it, while replacing, the replace
    /// field and its two ways.
    pub(super) fn render_search(&self, search: &FileSearch, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (spacing, radii) = (theme.spacing, theme.radii);
        let wash = hsla_alpha(s.text, alpha::FAINT);
        let bare = move |id: &'static str| {
            div()
                .id(id)
                .px(px(spacing.xs))
                .rounded(px(radii.xs))
                .cursor_pointer()
                .text_color(hsla(s.text_muted))
                .hover(move |st| st.bg(wash))
        };
        let count: SharedString = if search.query.needle.is_empty() {
            SharedString::default()
        } else if search.error.is_some() {
            "Not a valid pattern".into()
        } else if search.matches.is_empty() {
            "No matches".into()
        } else {
            let at = search.current.map_or(0, |c| c.saturating_add(1));
            let more = if search.matches.len() >= MATCHES_MAX { "+" } else { "" };
            format!("{at}/{}{more}", search.matches.len()).into()
        };
        let q = &search.query;
        let toggle = |id: &'static str, icon, label, on, which: fn(&mut Query) -> &mut bool| {
            crate::kit::icon_toggle(theme, id, icon, label, on, 1.0)
                .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _ev, _window, cx| this.toggle_query(which, cx)))
        };
        let find_row = div()
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .child(div().w(px(180.0)).child(Input::new(&search.input).aria_label("Find in file")))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(toggle(
                        "file-search-case",
                        IconName::CaseSensitive,
                        MATCH_CASE,
                        q.match_case,
                        |q| &mut q.match_case,
                    ))
                    .child(toggle(
                        "file-search-word",
                        IconName::WholeWord,
                        WHOLE_WORD,
                        q.whole_word,
                        |q| &mut q.whole_word,
                    ))
                    .child(toggle("file-search-regex", IconName::Regex, REGEX, q.regex, |q| {
                        &mut q.regex
                    })),
            )
            .child(
                div()
                    .id("file-search-count")
                    .min_w(px(40.0))
                    .text_color(hsla(if search.error.is_some() {
                        s.error
                    } else {
                        s.text_secondary
                    }))
                    .role(Role::Label)
                    .aria_label("Matches")
                    // A screen reader hears the pattern's own word on what is wrong.
                    .aria_value(
                        search.error.clone().map_or_else(|| count.clone(), SharedString::from),
                    )
                    .child(count),
            )
            .child(
                bare("file-search-prev")
                    .role(Role::Button)
                    .aria_label("Previous match")
                    .child("↑")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.step_hit(-1, cx))),
            )
            .child(
                bare("file-search-next")
                    .role(Role::Button)
                    .aria_label("Next match")
                    .child("↓")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.step_hit(1, cx))),
            )
            .child(
                bare("file-search-close")
                    .role(Role::Button)
                    .aria_label("Close find")
                    .child("✕")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.close_find(cx))),
            );
        let replace_row = search.replace.as_ref().map(|field| {
            div()
                .flex()
                .items_center()
                .gap(px(spacing.sm))
                .child(div().w(px(180.0)).child(Input::new(field).aria_label(REPLACE_PLACEHOLDER)))
                .child(
                    crate::kit::icon_button(
                        theme,
                        "file-replace-one",
                        IconName::Replace,
                        REPLACE_LINE,
                    )
                    .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _ev, window, cx| this.replace_one(window, cx))),
                )
                .child(
                    crate::kit::icon_button(
                        theme,
                        "file-replace-all",
                        IconName::ReplaceAll,
                        REPLACE_ALL,
                    )
                    .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _ev, window, cx| this.replace_all(window, cx))),
                )
        });
        div()
            .id("file-search")
            .debug_selector(|| "file-search".to_owned())
            .key_context(SEARCH_CTX)
            .absolute()
            .top(px(spacing.sm))
            .right(px(spacing.sm))
            .flex()
            .flex_col()
            .gap(px(spacing.xs))
            .px(px(spacing.sm))
            .py(px(spacing.xs))
            .rounded(px(radii.sm))
            .map(|el| crate::kit::elevate(el, theme))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text))
            .font_family(theme.typography.ui_family.clone())
            .map(|el| Self::search_keys(el, cx))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .role(Role::Group)
            .aria_label("Find in file")
            .child(find_row)
            .children(replace_row)
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
