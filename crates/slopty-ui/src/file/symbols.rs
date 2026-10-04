//! "Go to symbol" in a file tile: the names the file's grammar marks as defined
//! ([`crate::highlight::symbols`]), listed over the tile's top-right corner and narrowed by
//! what is typed, as the palette narrows (every word, any case).
//!
//! The list is read off the UI thread when it opens, since it parses the whole text. The caret
//! follows the chosen symbol; ↩ keeps it there, Esc puts it back.

use std::ops::Range;
use std::sync::Arc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement as _,
    MouseButton, ParentElement as _, ScrollStrategy, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, UniformListScrollHandle, Window, div, px, uniform_list,
};
use gpui_kit::component::input::{Input, InputEvent, InputState, RopeExt as _};

use super::{COLOURED_BYTES, CloseSymbols, FileView, NextSymbol, PreviousSymbol};
use crate::colors::hsla;
use crate::highlight::{self, Symbol, Syntax};

/// The field's placeholder.
pub(crate) const GO_TO_SYMBOL: &str = "Go to symbol";
/// What the list says while the file is read for its symbols.
pub(crate) const READING_SYMBOLS: &str = "Reading symbols\u{2026}";
/// What the list says of a file with no symbol its grammar marks.
pub(crate) const NO_SYMBOLS: &str = "No symbols";
/// The key context of the list; its keys bind in it.
pub const SYMBOLS_CTX: &str = "FileSymbols";
/// Rows the list shows before it scrolls.
const ROWS: usize = 10;

/// The open symbol list.
pub(super) struct Symbols {
    input: Entity<InputState>,
    /// Every symbol, once read.
    all: Option<Arc<[Symbol]>>,
    /// Indices into `all` of those the query keeps.
    shown: Vec<usize>,
    /// Index into `shown` of the chosen one.
    chosen: usize,
    /// The selection when it opened, put back by Esc.
    origin: Range<usize>,
    scroll: UniformListScrollHandle,
    _reading: Task<()>,
    _subscription: Subscription,
}

/// The indices of the symbols whose names every word of `query` finds, fuzzily
/// ([`crate::fuzzy`]): best first, ties in file order; all of them in file order for no query.
#[must_use]
fn narrowed(all: &[Symbol], query: &str) -> Vec<usize> {
    let mut fuzzy = crate::fuzzy::Fuzzy::new(query);
    let mut kept: Vec<(crate::fuzzy::Rank, usize)> =
        all.iter().enumerate().filter_map(|(ix, s)| Some((fuzzy.rank(&s.name, "")?, ix))).collect();
    kept.sort_by(|a, b| crate::fuzzy::best_first(&a.0, &b.0).then(a.1.cmp(&b.1)));
    kept.into_iter().map(|(_, ix)| ix).collect()
}

impl FileView {
    /// ⌘⇧O: list the file's symbols (the other corner fields close).
    pub fn go_to_symbol(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        self.goto = None;
        if self.search.take().is_some() {
            self.remark(cx);
        }
        if self.symbols.is_some() {
            return;
        }
        let first = self.editor.read(cx).text().slice_line(0).to_string();
        let text = self.text(cx);
        // Past the colouring's bound the parse takes seconds (0.6 s a MiB), so the file is
        // read as the plain text the tile shows it as.
        let syntax = (text.len() <= COLOURED_BYTES)
            .then(|| self.syntax.or_else(|| Syntax::for_path(&self.path, &first)))
            .flatten();
        let reading = cx.spawn(async move |this, cx| {
            let found: Arc<[Symbol]> = match syntax {
                Some(syntax) => cx
                    .background_spawn(async move { highlight::symbols(&text, syntax) })
                    .await
                    .into(),
                None => Arc::from(Vec::new()),
            };
            let _gone = this.update(cx, |this, cx| {
                let Some(list) = this.symbols.as_mut() else { return };
                list.all = Some(found);
                this.narrow(cx);
            });
        });
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(GO_TO_SYMBOL));
        let subscription =
            cx.subscribe_in(&input, window, |this, _input, event, window, cx| match event {
                InputEvent::Change => this.narrow(cx),
                InputEvent::PressEnter { .. } => this.close_symbols(false, window, cx),
                InputEvent::Focus | InputEvent::Blur => {}
            });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.symbols = Some(Symbols {
            input,
            all: None,
            shown: Vec::new(),
            chosen: 0,
            origin: self.editor.read(cx).selected_range(),
            scroll: UniformListScrollHandle::new(),
            _reading: reading,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Whether the symbol list is open.
    #[must_use]
    pub const fn listing_symbols(&self) -> bool {
        self.symbols.is_some()
    }

    /// The names the list shows now, in order; none while it is read.
    #[must_use]
    pub fn shown_symbols(&self) -> Option<Vec<&str>> {
        let list = self.symbols.as_ref()?;
        let all = list.all.as_ref()?;
        Some(list.shown.iter().filter_map(|&ix| all.get(ix)).map(|s| s.name.as_str()).collect())
    }

    /// Narrow the list to what is typed, choosing the first, the caret going to it.
    fn narrow(&mut self, cx: &mut Context<Self>) {
        let Some(list) = self.symbols.as_mut() else { return };
        let query = list.input.read(cx).value().to_string();
        list.shown = list.all.as_deref().map(|all| narrowed(all, &query)).unwrap_or_default();
        list.chosen = 0;
        self.show_chosen(cx);
    }

    /// ↓ (+1) and ↑ (−1) in the list, wrapping.
    pub fn step_symbol(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(list) = self.symbols.as_mut() else { return };
        let count = list.shown.len();
        if count == 0 {
            return;
        }
        let at = list.chosen.cast_signed().saturating_add(delta);
        list.chosen = at.rem_euclid(count.cast_signed()).cast_unsigned();
        self.show_chosen(cx);
    }

    /// The caret on the chosen symbol's name, and the list scrolled to it.
    fn show_chosen(&self, cx: &mut Context<Self>) {
        let Some(list) = self.symbols.as_ref() else { return };
        list.scroll.scroll_to_item(list.chosen, ScrollStrategy::Nearest);
        let symbol = list
            .shown
            .get(list.chosen)
            .and_then(|&ix| list.all.as_ref().and_then(|all| all.get(ix)))
            .cloned();
        if let Some(symbol) = symbol {
            self.editor.update(cx, |e, cx| {
                let text = e.text();
                let row = symbol.line.min(text.lines_len().saturating_sub(1));
                let start = text.line_start_offset(row).saturating_add(symbol.column);
                let end = start.saturating_add(symbol.name.len()).min(text.len());
                e.set_selected_range(start.min(end)..end, cx);
            });
        }
        cx.notify();
    }

    /// Close the list: ↩ keeps the caret on the symbol, Esc (`restore`) puts it back. The
    /// editor takes the keyboard again.
    pub fn close_symbols(&mut self, restore: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(list) = self.symbols.take() else { return };
        self.editor.update(cx, |e, cx| {
            if restore {
                let end = e.text().len();
                e.set_selected_range(list.origin.start.min(end)..list.origin.end.min(end), cx);
            }
            e.focus(window, cx);
        });
        cx.notify();
    }

    /// The list over the top-right corner: the field, then the symbols with their lines.
    pub(super) fn render_symbols(&self, list: &Symbols, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let (spacing, radii) = (theme.spacing, theme.radii);
        let row_h = theme.density.row;
        let note = |text: &'static str| {
            div().px(px(spacing.sm)).py(px(spacing.xs)).text_color(hsla(s.text_muted)).child(text)
        };
        let body = match &list.all {
            None => note(READING_SYMBOLS).into_any_element(),
            Some(_) if list.shown.is_empty() => note(NO_SYMBOLS).into_any_element(),
            Some(all) => {
                let all = Arc::clone(all);
                let shown = list.shown.clone();
                let chosen = list.chosen;
                let height = row_h * f32::from(u16::try_from(shown.len().min(ROWS)).unwrap_or(10));
                uniform_list(
                    "file-symbol-rows",
                    shown.len(),
                    cx.processor(move |this, range: Range<usize>, _window, _cx| {
                        range
                            .filter_map(|ix| {
                                let symbol = shown.get(ix).and_then(|&at| all.get(at))?;
                                let line: SharedString =
                                    symbol.line.saturating_add(1).to_string().into();
                                Some(
                                    div()
                                        .id(ix)
                                        .h(px(row_h))
                                        .flex()
                                        .items_center()
                                        .gap(px(spacing.sm))
                                        .px(px(spacing.sm))
                                        .rounded(px(radii.xs))
                                        .role(Role::ListItem)
                                        .aria_label(SharedString::from(symbol.name.clone()))
                                        .when(ix == chosen, |el| {
                                            el.bg(hsla(s.hover)).aria_selected(true)
                                        })
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .whitespace_nowrap()
                                                .font_family(theme_mono(&this.theme))
                                                .child(SharedString::from(symbol.name.clone())),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .text_color(hsla(s.text_muted))
                                                .child(line),
                                        ),
                                )
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&list.scroll)
                .h(px(height))
                .w_full()
                .into_any_element()
            }
        };
        div()
            .id("file-symbols")
            .debug_selector(|| "file-symbols".to_owned())
            .key_context(SYMBOLS_CTX)
            .absolute()
            .top(px(spacing.sm))
            .right(px(spacing.sm))
            .w(px(320.0))
            .flex()
            .flex_col()
            .gap(px(spacing.xs))
            .p(px(spacing.xs))
            .rounded(px(radii.sm))
            .map(|el| crate::kit::elevate(el, theme))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text))
            .font_family(theme.typography.ui_family.clone())
            .on_action(cx.listener(|this, _: &CloseSymbols, window, cx| {
                this.close_symbols(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextSymbol, _window, cx| this.step_symbol(1, cx)))
            .on_action(
                cx.listener(|this, _: &PreviousSymbol, _window, cx| this.step_symbol(-1, cx)),
            )
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .role(Role::Dialog)
            .aria_label(GO_TO_SYMBOL)
            .child(div().px(px(spacing.xs)).child(Input::new(&list.input).aria_label(GO_TO_SYMBOL)))
            .child(
                div().id("file-symbol-list").role(Role::ListBox).aria_label("Symbols").child(body),
            )
            .into_any_element()
    }
}

/// The terminal's mono family, which the editor draws code in.
fn theme_mono(theme: &slopty_theme::Theme) -> SharedString {
    theme.typography.mono_families.first().cloned().map(SharedString::from).unwrap_or_default()
}
