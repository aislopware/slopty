//! One find bar for every surface that finds in what it shows: a terminal, a file tile, a
//! thread and a page.
//!
//! After Ely GPUI Components (`src/editor/find.rs`), Copyright (c) 2026 Ely GPUI Component
//! contributors, MIT OR Apache-2.0: one widget whose toggles and replace row show only where
//! the owner handles them, a bad pattern's word in the count's place, and one wording for every
//! surface. The owner places it and binds its keys round it; the bar draws the field, the
//! toggles, the tally, the steps and the close, and hands each press back.

use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Entity, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_kit::component::input::{Input, InputState};
use regex::{Regex, RegexBuilder};
use slopty_theme::Theme;

use crate::colors::hsla;
use crate::icons::{IconSize, Symbol};

/// What a find bar says before anything is typed.
pub const PLACEHOLDER: &str = "Find";
/// What a replace field says before anything is typed.
pub const REPLACE_PLACEHOLDER: &str = "Replace";
/// The case toggle's name, as a screen reader, the hint and the palette say it.
pub const MATCH_CASE: &str = "Match case";
/// The whole-word toggle's name.
pub const WHOLE_WORD: &str = "Match whole word";
/// The pattern toggle's name.
pub const REGEX: &str = "Use regular expression";
/// The button that replaces the match on show.
pub const REPLACE: &str = "Replace";

/// Match case's face: the letters it tells apart.
pub const CASE_FACE: &str = "Aa";

/// Whole word's face.
pub const WORD_FACE: &str = "W";

/// A pattern's face: what a regular expression is written with.
pub const REGEX_FACE: &str = ".*";
/// The button that replaces every match.
pub const REPLACE_ALL: &str = "Replace all";
/// What the tally says when nothing matches.
pub const NO_MATCHES: &str = "No matches";
/// What the tally says for a pattern that does not compile; the pattern's own word is its
/// accessible value.
pub const BAD_PATTERN: &str = "Not a valid pattern";
/// What the tally says while matches are still being looked for elsewhere.
pub const FINDING: &str = "Finding\u{2026}";

/// The bar's width: room for a word or two, the toggles, the tally and the steps.
pub const WIDTH: f32 = 360.0;

/// The compiled program a pattern may grow to: a pathological one says it is too large instead
/// of taking the UI thread's memory (the crate's default is 10 MiB).
const PROGRAM_BYTES: usize = 4 << 20;

/// What a find bar asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// What was typed.
    pub needle: String,
    /// Case as typed. Off is smart case: a needle with no capital matches in any case, one
    /// with a capital as typed.
    pub match_case: bool,
    /// Only where the match starts and ends at a word's edge.
    pub whole_word: bool,
    /// The needle is a regular expression (Rust's `regex` syntax).
    pub regex: bool,
}

impl Query {
    /// The query as one regular expression in Rust's syntax, its case rule written in as a
    /// flag, so whoever compiles it (a worker searching a terminal's history) matches as the
    /// bar says whatever its own default. `None` for an empty needle.
    #[must_use]
    pub fn source(&self) -> Option<String> {
        if self.needle.is_empty() {
            return None;
        }
        let pattern = if self.regex { self.needle.clone() } else { regex::escape(&self.needle) };
        let pattern = if self.whole_word { format!(r"\b(?:{pattern})\b") } else { pattern };
        let exact = self.match_case || self.needle.chars().any(char::is_uppercase);
        Some(format!("(?{}){pattern}", if exact { "m-i" } else { "im" }))
    }

    /// The matcher, `None` for an empty needle, or why the pattern does not compile.
    ///
    /// # Errors
    /// The regular expression's own message, for a pattern it refuses.
    pub fn matcher(&self) -> Result<Option<Regex>, String> {
        let Some(source) = self.source() else { return Ok(None) };
        RegexBuilder::new(&source)
            .size_limit(PROGRAM_BYTES)
            .build()
            .map(Some)
            .map_err(|e| first_line(&e.to_string()))
    }

    /// Flip one of its toggles.
    pub const fn flip(&mut self, toggle: Toggle) {
        let on = match toggle {
            Toggle::MatchCase => &mut self.match_case,
            Toggle::WholeWord => &mut self.whole_word,
            Toggle::Regex => &mut self.regex,
        };
        *on = !*on;
    }
}

/// A regex error's own first line, without the pattern echoed above it.
fn first_line(error: &str) -> String {
    error
        .lines()
        .rev()
        .find(|l| l.starts_with("error:"))
        .unwrap_or("not a valid pattern")
        .to_owned()
}

/// One of a query's toggles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Toggle {
    /// [`Query::match_case`].
    MatchCase,
    /// [`Query::whole_word`].
    WholeWord,
    /// [`Query::regex`].
    Regex,
}

/// What the tally says.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Tally {
    /// Nothing typed.
    Quiet,
    /// Matches are still being looked for.
    Finding,
    /// The pattern does not compile: the pattern's own word on why.
    Bad(String),
    /// The matches: the one on show (from 0, when the owner knows it) among `total`, `more`
    /// past a cap.
    Found {
        /// The match on show.
        at: Option<usize>,
        /// How many.
        total: usize,
        /// There are more than `total`.
        more: bool,
    },
}

impl Tally {
    /// The tally as the bar shows it.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::Quiet => String::new(),
            Self::Finding => FINDING.to_owned(),
            Self::Bad(_) => BAD_PATTERN.to_owned(),
            Self::Found { total: 0, .. } => NO_MATCHES.to_owned(),
            Self::Found { at: Some(at), total, more } => {
                format!("{} of {total}{}", at.saturating_add(1), if *more { "+" } else { "" })
            }
            // Where the owner cannot say which match is on show (a page's own find).
            Self::Found { at: None, total: 1, more: false } => "1 match".to_owned(),
            Self::Found { at: None, total, more } => {
                format!("{total}{} matches", if *more { "+" } else { "" })
            }
        }
    }
}

type OnToggle = Rc<dyn Fn(Toggle, &mut Window, &mut App)>;
type OnStep = Rc<dyn Fn(i8, &mut Window, &mut App)>;
type OnReplace = Rc<dyn Fn(bool, &mut Window, &mut App)>;
type OnClose = Rc<dyn Fn(&mut Window, &mut App)>;

/// The find bar, under `id`: its parts are `{id}-count`, `{id}-previous`, `{id}-next`,
/// `{id}-close`, the toggles `{id}-case`, `{id}-word` and `{id}-regex`, and the replace row's
/// `{id}-replace` and `{id}-replace-all`.
#[derive(IntoElement)]
pub struct FindBar {
    id: SharedString,
    label: SharedString,
    field: Entity<InputState>,
    tally: Tally,
    query: Option<(Query, OnToggle)>,
    replace: Option<(Entity<InputState>, OnReplace)>,
    on_step: Option<OnStep>,
    on_close: Option<OnClose>,
    theme: Theme,
}

impl std::fmt::Debug for FindBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FindBar")
            .field("id", &self.id)
            .field("tally", &self.tally)
            .finish_non_exhaustive()
    }
}

impl FindBar {
    /// A bar under `id`, named `label` to a screen reader, its query typed in `field`.
    #[must_use]
    pub fn new(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        field: &Entity<InputState>,
        tally: Tally,
        theme: &Theme,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            field: field.clone(),
            tally,
            query: None,
            replace: None,
            on_step: None,
            on_close: None,
            theme: theme.clone(),
        }
    }

    /// Show the query's toggles as `query` has them; a press says which flipped.
    #[must_use]
    pub fn toggles(
        mut self,
        query: &Query,
        on_toggle: impl Fn(Toggle, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.query = Some((query.clone(), Rc::new(on_toggle)));
        self
    }

    /// Show the replace row, its text in `field`; a press says whether it is every match.
    #[must_use]
    pub fn replace(
        mut self,
        field: &Entity<InputState>,
        on_replace: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.replace = Some((field.clone(), Rc::new(on_replace)));
        self
    }

    /// The steps: `-1` for the match before (up), `1` for the one after (down).
    #[must_use]
    pub fn on_step(mut self, on_step: impl Fn(i8, &mut Window, &mut App) + 'static) -> Self {
        self.on_step = Some(Rc::new(on_step));
        self
    }

    /// The close.
    #[must_use]
    pub fn on_close(mut self, on_close: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(on_close));
        self
    }
}

impl RenderOnce for FindBar {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let Self { id, label, field, tally, query, replace, on_step, on_close, theme } = self;
        let s = theme.surfaces;
        let part = |name: &str| SharedString::from(format!("{id}-{name}"));
        let words = tally.words();
        let bad = matches!(tally, Tally::Bad(_));
        let value = match &tally {
            Tally::Bad(why) => SharedString::from(why.clone()),
            _ => SharedString::from(words.clone()),
        };
        let toggles = query.map(|(query, on_toggle)| {
            let toggle = |name: &str, face, label, on, which| {
                let on_toggle = Rc::clone(&on_toggle);
                super::text_toggle(&theme, part(name), face, label, on)
                    .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                    .on_click(move |_ev, window, cx| on_toggle(which, window, cx))
            };
            div()
                .flex()
                .flex_none()
                .items_center()
                .child(toggle("case", CASE_FACE, MATCH_CASE, query.match_case, Toggle::MatchCase))
                .child(toggle("word", WORD_FACE, WHOLE_WORD, query.whole_word, Toggle::WholeWord))
                .child(toggle("regex", REGEX_FACE, REGEX, query.regex, Toggle::Regex))
        });
        let step = |name: &str, icon, label, delta: i8| {
            let on_step = on_step.clone();
            super::icon_button(&theme, part(name), icon, label)
                .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                .when_some(on_step, move |el, on_step| {
                    el.on_click(move |_ev, window, cx| on_step(delta, window, cx))
                })
        };
        let lead = |icon| {
            crate::icons::icon(&theme, icon, IconSize::Inline, hsla(s.text_muted))
                .flex_none()
                .size(px(theme.typography.icon()))
        };
        let find_row = div()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .child(lead(Symbol::Magnifyingglass))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&field).appearance(false).aria_label(label.clone())),
            )
            .children(toggles)
            .child(
                super::tabular(div())
                    .id(part("count"))
                    .debug_selector({
                        let id = part("count");
                        move || id.to_string()
                    })
                    .flex_none()
                    .text_color(hsla(if bad { s.error } else { s.text_muted }))
                    .role(Role::Label)
                    .aria_label("Matches")
                    .aria_value(value)
                    .child(SharedString::from(words)),
            )
            .child(step("previous", Symbol::ChevronUp, "Previous match", -1))
            .child(step("next", Symbol::ChevronDown, "Next match", 1))
            .children(on_close.map(|on_close| {
                super::icon_button(&theme, part("close"), Symbol::Xmark, "Close find")
                    .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                    .on_click(move |_ev, window, cx| on_close(window, cx))
            }));
        let replace_row =
            replace.map(|(field, on_replace)| {
                // Replacing is said in words: a glyph for "replace" and one for "replace all"
                // had to be told apart by a hint.
                let button = |name: &str, label: &'static str, all: bool| {
                    let on_replace = Rc::clone(&on_replace);
                    let id = part(name);
                    let selector = id.to_string();
                    let el = div()
                        .id(id)
                        .debug_selector(move || selector)
                        .role(Role::Button)
                        .aria_label(label)
                        .flex_none()
                        .h(px(super::icon_button_side(&theme)))
                        .px(px(theme.spacing.sm))
                        .flex()
                        .items_center()
                        .rounded(px(theme.radii.sm))
                        .text_size(px(theme.typography.small()))
                        .font_weight(gpui::FontWeight(slopty_theme::Typography::MEDIUM_WEIGHT))
                        .text_color(hsla(s.text_secondary))
                        .cursor_pointer()
                        .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
                        .active(move |el| el.bg(hsla(s.pressed)))
                        .child(label);
                    crate::a11y::tab_stop(el, s.focus)
                        .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                        .on_click(move |_ev, window, cx| on_replace(all, window, cx))
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    // The replacement lines up under the query, past an empty lead: a word says
                    // what the row does, and no symbol says replace.
                    .child(div().flex_none().size(px(theme.typography.icon())))
                    .child(div().flex_1().min_w_0().child(
                        Input::new(&field).appearance(false).aria_label(REPLACE_PLACEHOLDER),
                    ))
                    .child(button("replace", REPLACE, false))
                    .child(button("replace-all", REPLACE_ALL, true))
            });
        let selector = id.to_string();
        // The bar's own hairline of room round its buttons, which sit flush to its edge.
        let rim = theme.spacing.xxs;
        super::elevate(div(), &theme)
            .id(id)
            .debug_selector(move || selector)
            .role(Role::Search)
            .aria_label(label)
            .w(px(WIDTH))
            .max_w_full()
            .flex()
            .flex_col()
            .gap(px(rim))
            .pl(px(theme.spacing.sm))
            .pr(px(rim))
            .py(px(rim))
            .rounded(px(theme.radii.lg))
            .text_size(px(theme.typography.small()))
            .font_family(theme.typography.ui_family.clone())
            .text_color(hsla(s.text))
            // Over what it finds in: the pointer there is the bar's, not the text's.
            .occlude()
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .child(find_row)
            .children(replace_row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(needle: &str) -> Query {
        Query { needle: needle.to_owned(), ..Query::default() }
    }

    /// The source carries its case rule, so a matcher built case-blind elsewhere still
    /// matches as the bar says.
    #[test]
    fn the_source_carries_its_case() {
        let exact = Query { match_case: true, whole_word: true, ..query("a.b") };
        let source = exact.source().unwrap();
        let blind = RegexBuilder::new(&source).case_insensitive(true).build().unwrap();
        assert!(blind.is_match("x a.b y") && !blind.is_match("A.B") && !blind.is_match("a.bc"));
        let any = query("ab").source().unwrap();
        let strict = RegexBuilder::new(&any).build().unwrap();
        assert!(strict.is_match("AB"));
    }

    #[test]
    fn the_tally_reads_one_way_everywhere() {
        assert_eq!(Tally::Quiet.words(), "");
        assert_eq!(Tally::Found { at: Some(1), total: 4, more: false }.words(), "2 of 4");
        assert_eq!(Tally::Found { at: Some(0), total: 10, more: true }.words(), "1 of 10+");
        assert_eq!(Tally::Found { at: None, total: 0, more: false }.words(), NO_MATCHES);
        assert_eq!(Tally::Found { at: None, total: 1, more: false }.words(), "1 match");
        assert_eq!(Tally::Found { at: None, total: 3, more: false }.words(), "3 matches");
        assert_eq!(Tally::Bad("error: x".into()).words(), BAD_PATTERN);
    }
}
