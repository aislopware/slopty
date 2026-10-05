//! A thing's own menu, opened by a right click or a long press on it (`kit::menu_press`) and hung
//! where the press landed: a tile's, from its navigator row or its header; a project's, from its
//! navigator header; a machine's, from its navigator row (its "…" menu, at the press).
//!
//! The bar's menu machinery draws it ([`MenuKind::Context`]), so it closes, takes the keyboard
//! and gives it back as every bar menu does. Its rows are worked out at the press, from what the
//! thing is then. Each runs what a key or a palette line already runs, so the menu is a way to
//! reach them and not a place of its own.

use std::rc::Rc;

use gpui::{App, Context, Pixels, Point, SharedString, Window};
use slopty_client::groups::GroupKey;
use slopty_client::layout::{Layout, TileRef, WorkerKey};
use slopty_proto::items::ItemKind;

use super::actions::{CloseItem, FullscreenTile, RenameItem};
use super::faces::Face;
use super::titlebar::MenuKind;
use super::{MenuEntry, MenuGroup, WorkspaceView};
use crate::draw::Draw;

/// The menu a press opened: its name, as a screen reader says it, and its rows.
#[derive(Clone, Debug)]
pub(super) struct ContextMenu {
    pub name: &'static str,
    pub entries: Vec<MenuEntry>,
}

/// Where a tile's menu was opened from, which decides a row or two of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Pressed {
    /// Its navigator row: Open leads, as the tile may be out of view.
    Navigator,
    /// Its own header.
    Header,
    /// Its tab in a tabbed column: what to do to the column's other tabs, too.
    Tab,
}

/// A tile's menu, as a screen reader names it.
pub(super) const TILE_MENU: &str = "Tile";

/// A project's menu, as a screen reader names it.
pub(super) const PROJECT_MENU: &str = "Project";

/// Copies where a tile is.
pub(super) const COPY_PATH: &str = "Copy path";

/// Keys beside a row where there is a keyboard with a ⌘ key.
const HINTS: bool = cfg!(target_os = "macos");

/// What a row runs, on the workspace itself.
type Run = Rc<dyn Fn(&mut WorkspaceView, &mut Window, &mut Context<WorkspaceView>)>;

impl WorkspaceView {
    /// Open `which` hung at `at`, where a press landed, rather than from its button.
    pub(super) fn open_menu_at(
        &mut self,
        which: MenuKind,
        at: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = Some(which);
        self.menu_at = Some(at);
        self.menu_keyed = window.last_input_was_keyboard();
        cx.notify();
    }

    /// Open `menu` at `at`.
    fn open_context_menu(
        &mut self,
        menu: ContextMenu,
        at: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = Some(menu);
        self.open_menu_at(MenuKind::Context, at, window, cx);
    }

    /// `el` opening `tile`'s menu on a right click or a long press, as [`Pressed`] there.
    pub(super) fn tile_menu_press<E>(
        el: E,
        tile: TileRef,
        pressed: Pressed,
        cx: &Draw<'_, Self>,
    ) -> E
    where
        E: gpui::InteractiveElement + gpui::ParentElement + gpui::Styled,
    {
        let this = cx.weak_entity();
        crate::kit::menu_press(el, move |at, window, cx| {
            let _gone = this.update(cx, |this, cx| {
                let entries = this.tile_entries(tile, pressed, cx);
                if !entries.is_empty() {
                    let menu = ContextMenu { name: TILE_MENU, entries };
                    this.open_context_menu(menu, at, window, cx);
                }
            });
        })
    }

    /// `el` opening a project's menu: a shell in its clone where it has one, and folding it.
    pub(super) fn project_menu_press<E>(
        el: E,
        key: GroupKey,
        new_shell: Option<(WorkerKey, String)>,
        cx: &Draw<'_, Self>,
    ) -> E
    where
        E: gpui::InteractiveElement + gpui::ParentElement + gpui::Styled,
    {
        let this = cx.weak_entity();
        crate::kit::menu_press(el, move |at, window, cx| {
            let _gone = this.update(cx, |this, cx| {
                let entries = this.project_entries(&key, new_shell.clone(), cx);
                let menu = ContextMenu { name: PROJECT_MENU, entries };
                this.open_context_menu(menu, at, window, cx);
            });
        })
    }

    /// `el` opening `key`'s machine menu, the one its "…" opens, at the press.
    pub(super) fn machine_menu_press<E>(el: E, key: WorkerKey, cx: &Draw<'_, Self>) -> E
    where
        E: gpui::InteractiveElement + gpui::ParentElement + gpui::Styled,
    {
        let this = cx.weak_entity();
        crate::kit::menu_press(el, move |at, window, cx| {
            let _gone = this.update(cx, |this, cx| {
                this.open_menu_at(MenuKind::Machine(key), at, window, cx);
            });
        })
    }

    /// `tile`'s rows: Open (from the navigator), Rename, Fullscreen, Move out of the column (from
    /// a tab), Copy path where it has one, then Close and, from a tab, Close other tabs.
    fn tile_entries(&self, tile: TileRef, pressed: Pressed, cx: &Context<Self>) -> Vec<MenuEntry> {
        let Some(item) = self.item(tile) else { return Vec::new() };
        let path = tile_path(&item.kind).or_else(|| self.cwd_of(item));
        let mut rows: Vec<(MenuGroup, &'static str, Option<&dyn gpui::Action>, Run)> = Vec::new();
        if pressed == Pressed::Navigator {
            rows.push((
                MenuGroup::Navigation,
                "Open",
                None,
                Rc::new(move |this, _w, cx| this.go_to_tile(tile, cx)),
            ));
        }
        rows.push((
            MenuGroup::Navigation,
            "Rename",
            Some(&RenameItem),
            Rc::new(move |this, window, cx| this.start_rename(tile, window, cx)),
        ));
        // On a phone a column is the screen's width already: fullscreen would add nothing.
        if !self.phone {
            rows.push((
                MenuGroup::Navigation,
                "Fullscreen",
                Some(&FullscreenTile),
                Rc::new(move |this, _w, cx| {
                    this.focus_tile(tile, cx);
                    this.width_action(cx, Layout::toggle_fullscreen);
                }),
            ));
        }
        if pressed == Pressed::Tab {
            rows.push((
                MenuGroup::Navigation,
                "Move out of the column",
                None,
                Rc::new(move |this, _w, cx| {
                    this.focus_tile(tile, cx);
                    this.layout_action(cx, Layout::consume_or_expel_window_right);
                }),
            ));
        }
        if let Some(path) = path {
            rows.push((
                MenuGroup::Navigation,
                COPY_PATH,
                None,
                Rc::new(move |_this, _w, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.clone()));
                }),
            ));
        }
        rows.push((
            MenuGroup::Removal,
            super::tile::CLOSE_TILE,
            Some(&CloseItem),
            Rc::new(move |this, window, cx| this.close_tile(tile, window, cx)),
        ));
        if pressed == Pressed::Tab {
            rows.push((
                MenuGroup::Removal,
                "Close other tabs",
                None,
                Rc::new(move |this, window, cx| {
                    for other in this.column_of(tile).into_iter().filter(|t| *t != tile) {
                        this.close_tile(other, window, cx);
                    }
                    this.focus_tile(tile, cx);
                }),
            ));
        }
        let bindings = super::key_bindings();
        rows.into_iter()
            .map(|(group, label, bound, run)| {
                let detail = match bound {
                    Some(bound) if HINTS => crate::palette::keys_for(bound, &bindings),
                    _ => String::new(),
                };
                Self::entry(group, label, detail, run, cx)
            })
            .collect()
    }

    /// A phone's "…" leads with the focused tile's rows, which its header holds on a wider
    /// screen: the agent's other faces to show, then its menu's rows.
    pub(super) fn phone_tile_entries(&self, cx: &Context<Self>) -> Vec<MenuEntry> {
        let Some(tile) = self.focused() else { return Vec::new() };
        let mut entries = Vec::new();
        if let Some(ItemKind::Terminal { session }) = self.item(tile).map(|item| &item.kind) {
            let session = *session;
            let shown = self.tile_face(session);
            for face in self.faces_of(session).into_iter().filter(|face| *face != shown) {
                let run: Run = Rc::new(move |this, _w, cx| this.set_face(session, face, cx));
                entries.push(Self::entry(MenuGroup::Tile, show_face(face), String::new(), run, cx));
            }
        }
        entries.extend(self.tile_entries(tile, Pressed::Header, cx).into_iter().map(
            |mut entry| {
                entry.group = MenuGroup::Tile;
                entry
            },
        ));
        entries
    }

    /// A project's rows: a new shell in its clone, where it has one, and folding it.
    fn project_entries(
        &self,
        key: &GroupKey,
        new_shell: Option<(WorkerKey, String)>,
        cx: &Context<Self>,
    ) -> Vec<MenuEntry> {
        let mut entries = Vec::new();
        if let Some((worker, cwd)) = new_shell {
            let run: Run = Rc::new(move |this, _w, cx| {
                this.open_session_on(worker, Some(cwd.clone()), Vec::new(), None, cx);
            });
            entries.push(Self::entry(MenuGroup::Tiles, "New shell here", String::new(), run, cx));
        }
        let folded = self.nav.folded.contains(key);
        let fold = key.clone();
        let run: Run = Rc::new(move |this, _w, cx| {
            if !this.nav.folded.remove(&fold) {
                this.nav.folded.insert(fold.clone());
            }
            cx.notify();
        });
        let label = if folded { "Unfold" } else { "Fold" };
        entries.push(Self::entry(MenuGroup::Navigation, label, String::new(), run, cx));
        entries
    }

    /// One row running `run` on the workspace.
    fn entry(
        group: MenuGroup,
        label: &'static str,
        detail: String,
        run: Run,
        cx: &Context<Self>,
    ) -> MenuEntry {
        let this = cx.entity().downgrade();
        MenuEntry {
            group,
            label: SharedString::from(label),
            detail: detail.into(),
            run: Rc::new(move |window: &mut Window, cx: &mut App| {
                let _gone = this.update(cx, |this, cx| run(this, window, cx));
            }),
        }
    }
}

impl WorkspaceView {
    /// The tiles of the column `tile` is in, top to bottom.
    fn column_of(&self, tile: TileRef) -> Vec<TileRef> {
        let Some(pos) = self.layout.position(tile) else { return Vec::new() };
        self.layout
            .workspaces()
            .get(pos.workspace)
            .and_then(|w| w.columns().get(pos.column))
            .map(|c| c.tiles().iter().map(slopty_client::layout::Tile::tile).collect())
            .unwrap_or_default()
    }
}

/// The row that shows `face` in its tile.
const fn show_face(face: Face) -> &'static str {
    match face {
        Face::Thread => "Show thread",
        Face::Terminal => "Show terminal",
        Face::Board => "Show board",
    }
}

/// Where a tile of `kind` is, when it is a place itself: a file's, a folder's, or the folder
/// whose changes it shows. A shell's or an agent's is where it works ([`WorkspaceView::cwd_of`]).
fn tile_path(kind: &ItemKind) -> Option<String> {
    match kind {
        ItemKind::File { path } | ItemKind::Folder { path } | ItemKind::Changes { path } => {
            Some(path.clone())
        }
        _ => None,
    }
}
