//! The surface's other scope: the open tiles rather than a worker's files.
//!
//! With the "Open tiles" chip on, the query goes to every tile open in the workspace, as each
//! tile's own ⌘F would take it, toggles included. The workspace asks the shells (their history
//! is on the worker) and counts the files it holds
//! ([`super::ProjectSearchEvent::FindTiles`]), then hands the tiles with a match back here
//! ([`ProjectSearch::set_tiles`]). A row is a tile and how many matches it holds; ↩ or a click goes
//! to that tile with its find bar open on the query.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, ElementId, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _,
    div, px, uniform_list,
};
use slopty_core::{ItemId, SessionId};

use super::{ProjectSearch, ProjectSearchEvent};
use crate::colors::hsla;
use crate::kit::find::Query;

/// What the surface searches.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SearchScope {
    /// The files under a directory on a worker.
    #[default]
    Files,
    /// The tiles open in the workspace.
    Tiles,
}

/// A tile the query matched in, as its row shows it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TileHit {
    /// The tile's title.
    pub title: String,
    /// How many matches it holds: lines for a file, hits for a shell.
    pub total: u32,
    /// Where ↩ goes.
    pub open: TileOpen,
}

/// How a tile is gone to from its row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TileOpen {
    /// A shell, its find bar opened on the query.
    Session(SessionId),
    /// A file tile, its find bar opened on the query.
    File(ItemId),
}

/// The tiles found for the query, and the one selected.
#[derive(Debug, Default)]
pub(super) struct Tiles {
    hits: Vec<TileHit>,
    at: usize,
}

/// What the scope chip says, and its pieces' names.
pub(crate) const FILES: &str = "Files";
pub(crate) const OPEN_TILES: &str = "Open tiles";
/// What the files row says while the open tiles are searched.
const EVERY_TILE: &str = "Every tile open in the workspace";

impl ProjectSearch {
    /// What it searches.
    #[must_use]
    pub const fn search_scope(&self) -> SearchScope {
        self.within
    }

    /// Search the files or the open tiles, and search again there.
    pub fn set_scope(&mut self, scope: SearchScope, cx: &mut Context<Self>) {
        if self.within == scope {
            return;
        }
        if scope == SearchScope::Tiles {
            self.stop(cx);
        }
        self.within = scope;
        self.results = None;
        self.refresh();
        self.tiles = Tiles::default();
        self.run(cx);
        cx.notify();
    }

    /// The query as a tile's find bar takes it.
    pub(super) fn tile_query(&self, cx: &App) -> Query {
        Query {
            needle: self.query.read(cx).value().to_string(),
            match_case: self.match_case,
            whole_word: self.whole_word,
            regex: self.regex,
        }
    }

    /// The tiles the query is in, in the workspace's reading order: the selection stays on
    /// the tile it was on, wherever it lands.
    pub fn set_tiles(&mut self, hits: Vec<TileHit>, cx: &mut Context<Self>) {
        let on = self.tiles.hits.get(self.tiles.at).map(|h| h.open);
        self.tiles.at = on.and_then(|on| hits.iter().position(|h| h.open == on)).unwrap_or(0);
        self.tiles.hits = hits;
        cx.notify();
    }

    /// The tiles found, for a test or the dump.
    #[must_use]
    pub fn tile_hits(&self) -> &[TileHit] {
        &self.tiles.hits
    }

    pub(super) const fn tiles_shown(&self) -> bool {
        !self.tiles.hits.is_empty()
    }

    /// Move the selection `delta` tiles, stopping at the ends.
    pub(super) fn step_tile(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(last) = self.tiles.hits.len().checked_sub(1) else { return };
        self.tiles.at = self.tiles.at.saturating_add_signed(delta).min(last);
        self.scroll.scroll_to_item(self.tiles.at, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Go to the selected tile.
    pub(super) fn open_tile(&self, cx: &mut Context<Self>) {
        let Some(hit) = self.tiles.hits.get(self.tiles.at) else { return };
        tracing::info!(open = ?hit.open, "search goes to a tile");
        cx.emit(ProjectSearchEvent::OpenTile { open: hit.open, query: self.tile_query(cx) });
    }

    /// What the foot says of the tiles found.
    pub(super) fn tile_status(&self, cx: &App) -> Option<String> {
        if self.query.read(cx).value().is_empty() {
            return None;
        }
        Some(match self.tiles.hits.len() {
            0 => super::NO_RESULTS.to_owned(),
            1 => "1 tile".to_owned(),
            n => format!("{n} tiles"),
        })
    }

    /// The scope chip: the files or the open tiles, the chosen one raised.
    pub(super) fn scope_chip(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let piece = |id: &'static str, label: &'static str, scope: SearchScope| {
            let on = self.within == scope;
            let el = div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::RadioButton)
                .aria_label(label)
                .aria_toggled(if on {
                    gpui::accesskit::Toggled::True
                } else {
                    gpui::accesskit::Toggled::False
                })
                .px(px(theme.spacing.sm))
                .py(px(theme.spacing.xxs))
                .rounded(px(theme.radii.sm))
                .cursor_pointer()
                .text_color(hsla(if on { s.text } else { s.text_secondary }))
                .when(!on, |el| el.hover(move |el| el.bg(hsla(s.hover))))
                .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _ev, _window, cx| this.set_scope(scope, cx)))
                .child(label);
            let el = if on { crate::kit::selected(el, theme, true) } else { el };
            crate::a11y::tab_stop(el, s.focus)
        };
        div()
            .id("search-scope")
            .debug_selector(|| "search-scope".to_owned())
            .role(Role::RadioGroup)
            .aria_label("Search in")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xxs))
            .child(piece("search-scope-files", FILES, SearchScope::Files))
            .child(piece("search-scope-tiles", OPEN_TILES, SearchScope::Tiles))
            .into_any_element()
    }

    /// What the files row holds while the tiles are searched: what is searched, in words.
    pub(super) fn every_tile(&self) -> AnyElement {
        crate::kit::meta(div(), &self.theme)
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .child(EVERY_TILE)
            .into_any_element()
    }

    /// The tiles found, a row each.
    pub(super) fn tile_list(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let pad = crate::palette::list_pad(theme);
        let rows = uniform_list(
            "search-tiles",
            self.tiles.hits.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                range.filter_map(|ix| this.tile_row(ix, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .size_full()
        .p(px(pad));
        div()
            .id("search-list")
            .debug_selector(|| "search-list".to_owned())
            .role(Role::ListBox)
            .aria_label("Results")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(self.plate.under(theme))
            .child(rows)
            .into_any_element()
    }

    /// Row `ix`: the tile's title, and its count.
    fn tile_row(&self, ix: usize, cx: &Context<Self>) -> Option<AnyElement> {
        let hit = self.tiles.hits.get(ix)?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let chosen = self.tiles.at == ix;
        let pad = crate::palette::list_pad(theme);
        let icon = match hit.open {
            TileOpen::Session(_) => crate::icons::Symbol::Terminal,
            TileOpen::File(_) => crate::icons::Symbol::DocText,
        };
        let total = usize::try_from(hit.total).unwrap_or(usize::MAX);
        let label = format!("{}, {}", hit.title, super::count_label(total));
        let el = div()
            .id(ElementId::NamedInteger("search-tile".into(), u64::try_from(ix).unwrap_or(0)))
            .debug_selector(move || format!("search-tile-{ix}"))
            .role(Role::ListBoxOption)
            .aria_label(SharedString::from(label))
            .aria_selected(chosen)
            .w_full()
            .h(px(crate::palette::line_height(theme)))
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .px(px(theme.spacing.inset() - pad))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .active(move |st| st.bg(hsla(s.pressed)))
            .on_mouse_move(cx.listener(move |this, _ev, _window, cx| {
                if this.tiles.at != ix {
                    this.tiles.at = ix;
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.tiles.at = ix;
                this.open_tile(cx);
            }))
            .child(crate::icons::icon(
                theme,
                icon,
                crate::icons::IconSize::Inline,
                hsla(s.text_muted),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(if chosen { s.text } else { s.text_secondary }))
                    .child(SharedString::from(hit.title.clone())),
            )
            .child(
                crate::kit::tabular(crate::kit::pill(theme, s.text_secondary))
                    .flex_none()
                    .child(SharedString::from(hit.total.to_string())),
            );
        Some(if chosen {
            self.plate.mark(el, ix).into_any_element()
        } else {
            el.into_any_element()
        })
    }
}
