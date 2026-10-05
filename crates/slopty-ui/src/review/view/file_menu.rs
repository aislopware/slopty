//! A review file's own menu, by a right click or a long press on its row in the list or on its
//! head in the diff, hung where the press landed.
//!
//! - **Keep**, on a thread's own review, as its head's Keep does.
//! - **Open**: the file in a tile of its own on its machine ([`ReviewEvent::OpenFile`]).
//! - **Open in the person's editor**, where that would open something.
//! - **Copy path**: the file's whole path on its machine.
//! - **Revert**, set apart last. Nothing undoes it, so the row says what it does before it acts:
//!   the file by name and the point it goes back to ([`revert_words`]).
//!
//! The review names its files from the repository's root, which need not be the thread's
//! folder, so the tile reads the repository's root as it opens ([`ReviewView::root`]). Until
//! it is known, the two opens are left out and the copy says it copies the path in the
//! repository.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, FocusHandle, IntoElement as _, ParentElement as _, Pixels, Point,
    Window, px,
};

use super::{ReviewEvent, ReviewView};
use crate::kit;
use crate::review::model::Scope;

/// Copies the file's whole path on its machine.
pub const COPY_PATH: &str = "Copy path";

/// Copies the file's path from the repository's root, while the root is not known.
pub const COPY_PATH_IN_REPOSITORY: &str = "Copy path in the repository";

/// The menu open over a file.
#[derive(Clone, Debug)]
pub(in crate::review) struct FileMenu {
    /// The file, by its place in the review.
    at: usize,
    pos: Point<Pixels>,
    /// What held the keyboard before it opened, which has it back when it closes.
    back_to: Option<FocusHandle>,
}

/// What a row of the menu does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pick {
    Keep,
    Open,
    Editor,
    CopyPath,
    Revert,
}

/// What Revert says it does to the file `name` under `scope`: the point the file goes back to.
#[must_use]
pub fn revert_words(name: &str, scope: Scope) -> String {
    let to = match scope {
        Scope::LastTurn => "before the last turn",
        Scope::SinceReviewed => "where it was last reviewed",
        Scope::AllTurns => "before the thread's first turn",
        Scope::Uncommitted => "its last commit",
        Scope::WholeBranch => "the branch's base",
    };
    format!("Revert {name} to {to}")
}

impl ReviewView {
    /// `el`, the file at `at`'s row or head, opening its menu on a right click or a long press.
    pub(super) fn file_menu_press<E>(el: E, at: usize, cx: &Context<Self>) -> E
    where
        E: gpui::InteractiveElement + gpui::ParentElement + gpui::Styled,
    {
        let this = cx.entity().downgrade();
        kit::menu_press(el, move |pos, window, cx| {
            let _gone = this.update(cx, |v, cx| v.open_file_menu(at, pos, window, cx));
        })
    }

    fn open_file_menu(
        &mut self,
        at: usize,
        pos: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.model.file(at).is_none() {
            return;
        }
        let back_to =
            self.file_menu.take().and_then(|open| open.back_to).or_else(|| window.focused(cx));
        self.file_menu = Some(FileMenu { at, pos, back_to });
        cx.notify();
    }

    /// Close the menu, the keyboard back where it was.
    fn close_file_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.file_menu.take() {
            if let Some(back) = menu.back_to {
                window.focus(&back, cx);
            }
            cx.notify();
        }
    }

    /// The file at `at`'s whole path on its machine, once the repository's root is known.
    fn whole_path(&self, at: usize, cx: &App) -> Option<String> {
        let path = &self.model.file(at)?.path;
        let root = self.root(cx)?;
        Some(format!("{}/{path}", root.trim_end_matches('/')))
    }

    fn file_pick(&mut self, pick: Pick, at: usize, cx: &mut Context<Self>) {
        match pick {
            Pick::Keep => self.pick(at, None, true, cx),
            Pick::Revert => self.pick(at, None, false, cx),
            Pick::Open => {
                if let Some(path) = self.whole_path(at, cx) {
                    cx.emit(ReviewEvent::OpenFile { path });
                }
            }
            Pick::Editor => {
                use crate::file::open_with;
                let worker = self.hub.read(cx).worker().to_owned();
                let opening = self
                    .whole_path(at, cx)
                    .and_then(|path| open_with::opening_named(&worker, &path, None, cx));
                if let Some(opening) = opening {
                    open_with::open(&opening, cx);
                }
            }
            Pick::CopyPath => {
                let path = self
                    .whole_path(at, cx)
                    .or_else(|| self.model.file(at).map(|file| file.path.clone()));
                if let Some(path) = path {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(path));
                }
            }
        }
    }

    /// The open menu, drawn late where the press landed.
    pub(super) fn file_menu_panel(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let menu = self.file_menu.clone()?;
        let at = menu.at;
        let file = self.model.file(at)?;
        let name = file.path.rsplit('/').next().unwrap_or(&file.path).to_owned();
        let whole = self.whole_path(at, cx);
        // A removed file has nothing on its machine to open.
        let there = whole.clone().filter(|_| file.to.is_some());
        let editor = there
            .as_ref()
            .and_then(|path| {
                crate::file::open_with::opening_named(self.hub.read(cx).worker(), path, None, cx)
            })
            .and_then(|_| cx.try_global::<crate::file::open_with::Editors>())
            .and_then(crate::file::open_with::Editors::label);
        let picks = self.own().is_some() && self.picking(at, None, cx).is_none();
        let this = cx.entity().downgrade();
        let row = |pick: Pick, key: &'static str, label: String| {
            let this = this.clone();
            kit::MenuItem::new(key, label, move |_window, cx| {
                let _gone = this.update(cx, |v, cx| v.file_pick(pick, at, cx));
            })
        };
        let mut rows = kit::Menu::new();
        if picks {
            rows.push(row(Pick::Keep, "keep", "Keep".to_owned()));
        }
        if there.is_some() {
            rows.push(row(Pick::Open, "open", "Open".to_owned()));
        }
        if let Some(editor) = editor {
            rows.push(row(Pick::Editor, "editor", editor));
        }
        let copy = if whole.is_some() { COPY_PATH } else { COPY_PATH_IN_REPOSITORY };
        rows.push(row(Pick::CopyPath, "copy-path", copy.to_owned()));
        if picks {
            rows.separate();
            rows.push(row(Pick::Revert, "revert", revert_words(&name, self.scope)));
        }
        let panel = kit::MenuPanel::new("review-file-menu", "File", Rc::new(rows), &self.theme, {
            move |window: &mut Window, cx: &mut App| {
                let _gone = this.update(cx, |v, cx| v.close_file_menu(window, cx));
            }
        });
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(menu.pos)
                    .snap_to_window_with_margin(px(self.theme.spacing.sm))
                    .child(panel),
            )
            .with_priority(crate::palette::Layer::Submenu.priority())
            .into_any_element(),
        )
    }
}
