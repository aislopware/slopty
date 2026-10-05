//! A folder row's own menu, by a right click or a long press on it: the row is selected, and
//! the menu offers what the folder's keys and palette lines do to it, hung where the press
//! landed.
//!
//! - Open: a folder here, a file beside.
//! - Rename or move…, in the row.
//! - Download… on a Mac, Save to Files… on an iPhone or iPad.
//! - Open in the person's editor, where that would open something.
//! - Copy path.
//! - Move to Trash, set apart last.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, FocusHandle, IntoElement as _, ParentElement as _, Pixels, Point,
    Window, px,
};

use super::{DOWNLOAD, FILES_PICKER, FolderView, MOVE_TO_TRASH, RENAME_OR_MOVE, SAVE_TO_FILES};
use crate::kit;

/// Copies where the row's entry is.
const COPY_PATH: &str = "Copy path";

/// The menu open over a row.
#[derive(Clone, Debug)]
pub(super) struct RowMenu {
    at: Point<Pixels>,
    /// What held the keyboard before it opened, which has it back when it closes.
    back_to: Option<FocusHandle>,
}

/// What a row of the menu does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pick {
    Open,
    Rename,
    Save,
    Editor,
    CopyPath,
    Trash,
}

impl FolderView {
    /// `el`, row `ix`, selecting itself and opening its menu on a right click or a long press.
    pub(super) fn row_menu_press<E>(el: E, ix: usize, cx: &Context<Self>) -> E
    where
        E: gpui::InteractiveElement + gpui::ParentElement + gpui::Styled,
    {
        let this = cx.entity().downgrade();
        kit::menu_press(el, move |at, window, cx| {
            let _gone = this.update(cx, |v, cx| v.open_row_menu(ix, at, window, cx));
        })
    }

    fn open_row_menu(
        &mut self,
        ix: usize,
        at: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.naming.is_some() || self.entry_path(ix).is_none() {
            return;
        }
        self.select(ix, cx);
        let back_to =
            self.row_menu.take().and_then(|open| open.back_to).or_else(|| window.focused(cx));
        self.row_menu = Some(RowMenu { at, back_to });
        cx.notify();
    }

    /// Close the menu, the keyboard back where it was.
    fn close_row_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.row_menu.take() {
            if let Some(back) = menu.back_to {
                window.focus(&back, cx);
            }
            cx.notify();
        }
    }

    fn row_pick(&mut self, pick: Pick, window: &mut Window, cx: &mut Context<Self>) {
        match pick {
            Pick::Open => self.open_selected(cx),
            Pick::Rename => self.rename_selected(window, cx),
            Pick::Save => self.save_selected(cx),
            Pick::Editor => self.open_in_editor(cx),
            Pick::CopyPath => {
                if let Some((path, _)) = self.selected.and_then(|ix| self.entry_path(ix)) {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(path));
                }
            }
            Pick::Trash => self.trash_selected(cx),
        }
    }

    /// The open menu, drawn late where the press landed.
    pub(super) fn row_menu_panel(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let menu = self.row_menu.clone()?;
        let (path, _) = self.selected.and_then(|ix| self.entry_path(ix))?;
        let editor = crate::file::open_with::opening(self.worker, &path, None, cx)
            .and_then(|_| cx.try_global::<crate::file::open_with::Editors>())
            .and_then(crate::file::open_with::Editors::label);
        let save = if FILES_PICKER { SAVE_TO_FILES } else { DOWNLOAD };
        let this = cx.entity().downgrade();
        let row = |pick: Pick, key: &'static str, label: String| {
            let this = this.clone();
            kit::MenuItem::new(key, label, move |window, cx| {
                let _gone = this.update(cx, |v, cx| v.row_pick(pick, window, cx));
            })
        };
        let mut rows = kit::Menu::new()
            .item(row(Pick::Open, "open", "Open".to_owned()))
            .item(row(Pick::Rename, "rename", RENAME_OR_MOVE.to_owned()))
            .item(row(Pick::Save, "save", save.to_owned()));
        if let Some(editor) = editor {
            rows.push(row(Pick::Editor, "editor", editor));
        }
        rows.push(row(Pick::CopyPath, "copy-path", COPY_PATH.to_owned()));
        rows.separate();
        rows.push(row(Pick::Trash, "trash", MOVE_TO_TRASH.to_owned()));
        let panel =
            kit::MenuPanel::new("folder-menu", "Folder item", Rc::new(rows), &self.theme, {
                move |window: &mut Window, cx: &mut App| {
                    let _gone = this.update(cx, |v, cx| v.close_row_menu(window, cx));
                }
            });
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(menu.at)
                    .snap_to_window_with_margin(px(self.theme.spacing.sm))
                    .child(panel),
            )
            .with_priority(crate::palette::Layer::Submenu.priority())
            .into_any_element(),
        )
    }
}
