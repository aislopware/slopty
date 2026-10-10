//! A thing's own menu, opened by a right click or a long press on it (`kit::menu_press`) and hung
//! where the press landed: a tile's, from its navigator row or its header; a project's, from its
//! navigator header; a machine's, from its navigator row (its "…" menu, at the press); a title
//! tab's, from the tab (`MonoCode`'s `TitleBar` menu).
//!
//! The bar's menu machinery draws it ([`MenuKind::Context`]), so it closes, takes the keyboard
//! and gives it back as every bar menu does. Its rows are worked out at the press, from what the
//! thing is then. Each runs what a key or a palette line already runs, so the menu is a way to
//! reach them and not a place of its own.

use std::rc::Rc;

use gpui::{App, Context, Pixels, Point, SharedString, Window};
use slopty_client::groups::GroupKey;
use slopty_client::layout::{Drop, Side, Tab, TabId, TileRef, WorkerKey};
use slopty_proto::items::ItemKind;

use super::actions::{CloseItem, ReloadPage, RenameItem, ZoomPane};
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
    /// Its tab in a pane of several: what to do to the pane's other tabs, too.
    Tab,
}

/// A tile's menu, as a screen reader names it.
pub(super) const TILE_MENU: &str = "Tile";

/// A project's menu, as a screen reader names it.
pub(super) const PROJECT_MENU: &str = "Project";

/// A title tab's menu, as a screen reader names it.
pub(super) const TAB_MENU: &str = "Tab";

/// A title tab's menu's rows.
pub(super) const CLOSE_TAB: &str = super::title_tabs::CLOSE_TAB;
pub(super) const CLOSE_OTHER_TABS: &str = "Close other tabs";
pub(super) const CLOSE_TABS_RIGHT: &str = "Close tabs to the right";
pub(super) const CLOSE_TABS_LEFT: &str = "Close tabs to the left";

/// A pane tab's menu's row that closes the pane's other tiles: tiles, not the title bar's
/// tabs, which "Close other tabs" closes.
pub(super) const CLOSE_OTHER_TILES: &str = "Close other tiles";

/// A project's menu's rows that pin it above the rest, and mute its notifications.
pub(super) const PIN_TO_TOP: &str = "Pin to top";
pub(super) const UNPIN: &str = "Unpin";
pub(super) const MUTE_NOTES: &str = "Mute notifications";
pub(super) const UNMUTE_NOTES: &str = "Unmute notifications";

/// Copies where a tile is.
pub(super) const COPY_PATH: &str = "Copy path";

/// Loads a page tile's page again.
pub(super) const RELOAD_PAGE: &str = "Reload page";

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

    /// Open title tab `id`'s menu at `at`, where it was pressed.
    pub(super) fn open_title_tab_menu(
        &mut self,
        id: TabId,
        at: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.title_tab_entries(id, cx);
        if !entries.is_empty() {
            self.open_context_menu(ContextMenu { name: TAB_MENU, entries }, at, window, cx);
        }
    }

    /// Title tab `id`'s rows: close it, then the project's other tabs, those to its right and
    /// those to its left, each only while there are some. Each closes what the tabs hold as ⌘W
    /// would, a running shell asking first, and leaves tab `id` on show.
    fn title_tab_entries(&self, id: TabId, cx: &Context<Self>) -> Vec<MenuEntry> {
        let Some((p, t)) = self.layout.tab_place(id) else { return Vec::new() };
        let ids: Vec<TabId> = self
            .layout
            .projects()
            .get(p)
            .map(|project| project.tabs().iter().map(Tab::id).collect())
            .unwrap_or_default();
        let left: Vec<TabId> = ids.iter().take(t).copied().collect();
        let right: Vec<TabId> = ids.iter().skip(t.saturating_add(1)).copied().collect();
        let others: Vec<TabId> = left.iter().chain(&right).copied().collect();
        let close = |tabs: Vec<TabId>| -> Run {
            Rc::new(move |this, window, cx| {
                use super::title_tabs::TitleTabsHost as _;
                for tab in &tabs {
                    this.close_title_tab(*tab, window, cx);
                }
                this.layout_action(cx, |l| l.show_tab(id));
            })
        };
        let only = Rc::new(move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            use super::title_tabs::TitleTabsHost as _;
            this.close_title_tab(id, window, cx);
        });
        let mut rows: Vec<(&'static str, Run)> = vec![(CLOSE_TAB, only)];
        for (label, tabs) in
            [(CLOSE_OTHER_TABS, others), (CLOSE_TABS_RIGHT, right), (CLOSE_TABS_LEFT, left)]
        {
            if !tabs.is_empty() {
                rows.push((label, close(tabs)));
            }
        }
        rows.into_iter()
            .map(|(label, run)| Self::entry(MenuGroup::Removal, label, String::new(), run, cx))
            .collect()
    }

    /// `tile`'s rows: Open (from the navigator), from its header an agent's other faces to show
    /// and a page's reload, then Rename, Zoom pane, Split out to the right (from a tab), Copy
    /// path where it has one, then Close and, from a tab, Close other tiles.
    ///
    /// The header holds no button at rest and touch has no hover, so its long press is where a
    /// finger finds the face toggle and close; a page's reload is here and the palette's, as ⌘R
    /// is the layout's.
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
        if pressed == Pressed::Header {
            if let ItemKind::Terminal { session } = item.kind {
                let shown = self.tile_face(session);
                let faces = self.faces_of(session);
                for face in faces.into_iter().filter(|face| *face != shown) {
                    rows.push((
                        MenuGroup::Navigation,
                        show_face(face),
                        None,
                        Rc::new(move |this, _w, cx| {
                            this.focus_tile(tile, cx);
                            this.set_face(session, face, cx);
                        }),
                    ));
                }
            }
            if let ItemKind::Browser { .. } = item.kind {
                rows.push((
                    MenuGroup::Navigation,
                    RELOAD_PAGE,
                    None,
                    Rc::new(move |this, window, cx| {
                        this.focus_tile(tile, cx);
                        this.reload_page(&ReloadPage, window, cx);
                    }),
                ));
            }
        }
        rows.push((
            MenuGroup::Navigation,
            "Rename",
            Some(&RenameItem),
            Rc::new(move |this, window, cx| this.start_rename(tile, window, cx)),
        ));
        // On a phone a pane is the screen already: a zoom would add nothing.
        if !self.phone {
            rows.push((
                MenuGroup::Navigation,
                "Zoom pane",
                Some(&ZoomPane),
                Rc::new(move |this, window, cx| {
                    this.focus_tile(tile, cx);
                    this.toggle_zoom(window, cx);
                }),
            ));
        }
        if pressed == Pressed::Tab {
            rows.push((
                MenuGroup::Navigation,
                "Split out to the right",
                None,
                Rc::new(move |this, _w, cx| {
                    this.focus_tile(tile, cx);
                    let Some(pane) = this.layout.position(tile).map(|p| p.pane) else { return };
                    this.layout_action(cx, |l| {
                        l.place(tile, Drop { pane, edge: Some(Side::Right) });
                    });
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
                CLOSE_OTHER_TILES,
                None,
                Rc::new(move |this, window, cx| {
                    for other in this.pane_tiles(tile).into_iter().filter(|t| *t != tile) {
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

    /// A phone's "…" leads with the focused tile's rows, which its header's menu holds on a
    /// wider screen: the agent's other faces to show, then the rest of that menu.
    pub(super) fn phone_tile_entries(&self, cx: &Context<Self>) -> Vec<MenuEntry> {
        let Some(tile) = self.focused() else { return Vec::new() };
        self.tile_entries(tile, Pressed::Header, cx)
            .into_iter()
            .map(|mut entry| {
                entry.group = MenuGroup::Tile;
                entry
            })
            .collect()
    }

    /// A project's rows: a new shell in its clone, where it has one, folding it, pinning it
    /// above the rest and muting its notifications.
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
            let label = super::machines::NEW_SHELL_HERE;
            entries.push(Self::entry(MenuGroup::Tiles, label, String::new(), run, cx));
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
        let pinned = self.navigator().pinned.contains(key);
        let pin = key.clone();
        let run: Run = Rc::new(move |this, _w, cx| this.toggle_pinned(&pin, cx));
        let label = if pinned { UNPIN } else { PIN_TO_TOP };
        entries.push(Self::entry(MenuGroup::Navigation, label, String::new(), run, cx));
        let muted = self.navigator().muted.contains(key);
        let mute = key.clone();
        let run: Run = Rc::new(move |this, _w, cx| this.toggle_muted(&mute, cx));
        let label = if muted { UNMUTE_NOTES } else { MUTE_NOTES };
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
    /// The tiles of the pane `tile` is in, as its tabs run.
    pub(super) fn pane_tiles(&self, tile: TileRef) -> Vec<TileRef> {
        let Some(pos) = self.layout.position(tile) else { return Vec::new() };
        let project = self.layout.projects().get(pos.project);
        let tab = project.and_then(|p| p.tabs().iter().find(|t| t.id() == pos.tab));
        tab.and_then(|t| t.pane(pos.pane)).map(|p| p.tiles().to_vec()).unwrap_or_default()
    }
}

/// The row that shows `face` in its tile.
pub(super) const fn show_face(face: Face) -> &'static str {
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
        ItemKind::File { path } | ItemKind::Folder { path } | ItemKind::Changes { path, .. } => {
            Some(path.clone())
        }
        _ => None,
    }
}
