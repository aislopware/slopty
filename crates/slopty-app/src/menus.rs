//! The app's menus: the Mac's menu bar, and the main menu an iPad with a keyboard shows in its
//! menu bar and in the sheet a held ⌘ brings up (`gpui_ios`'s `UIMainMenuSystem`).
//!
//! Items name the same actions the key bindings do, so the shortcuts shown next to them come
//! from the keymap in effect whenever the menus are built ([`crate::set_app_menus`]).
//!
//! The File menu opens and saves what the palette does, by the same actions: Save is the file
//! tile's own, so it is greyed unless a file has the keyboard. Upload sends files picked here up
//! to the focused shell, folder or window; Download brings a folder tile's selected entry down.
//! On an iPad they are the Files picker's, and named for it, as the palette names them.
//!
//! The Edit menu names the text fields' own actions (gpui-kit's), which every field answers:
//! the editors, the composer and the settings' search, and the terminal as its own. Cut, Copy,
//! Paste and Select All also stand for the system's own commands, so a web page's view answers
//! them too, and on an iPad they take the place of UIKit's. An item nothing focused answers is
//! greyed, as the system greys it.
//!
//! The Mac's menu bar also has the application's own items (Services, Hide, Quit, which the app
//! passes in) and a Window menu; an iPad has neither, since iPadOS gives the app menu its own
//! items and arranges windows itself.

use gpui::{Menu, MenuItem, OsAction};

/// Every menu, after an application menu of Settings, Add Machine and then `app_items`. For an
/// iPad (`ios`) there is no Window menu, and Upload and Download are the Files picker's.
#[must_use]
pub fn menus(app_items: Vec<MenuItem>, ios: bool) -> Vec<Menu> {
    use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
    use slopty_ui::file::SaveFile;
    use slopty_ui::folder::{SAVE_TO_FILES, SaveToFiles, UPLOAD_FROM_FILES, UploadFromFiles};
    use slopty_ui::terminal::{Find, FindNext, FindPrev};
    use slopty_ui::workspace::{
        AddWindow, CloseItem, CloseOtherTabs, EqualizePanes, FocusDown, FocusLeft, FocusRight,
        FocusUp, FontLarger, FontReset, FontSmaller, GoBack, GoForward, MoveDown, MoveLeft,
        MoveRight, MoveToProject, MoveUp, NewAgent, NewNote, NewTerminal, NextAttention,
        NextProject, NextTab, OpenFile, OpenFolder, OpenPalette, OpenUrl, PreviousProject,
        PreviousTab, SaveCopy, SplitDown, SplitRight, StartAgent, TabTerminal, ToggleMute,
        ToggleStats, UndoClose, ZoomPane,
    };

    use crate::{Minimize, OpenHelp, ShowWindow, Zoom};
    let (upload, download) =
        if ios { (UPLOAD_FROM_FILES, SAVE_TO_FILES) } else { ("Upload…", "Download…") };
    let app = [
        MenuItem::action("About Slopty", crate::OpenAbout),
        MenuItem::separator(),
        MenuItem::action("Settings…", crate::OpenSettings),
        MenuItem::separator(),
        MenuItem::action("Add Machine…", crate::AddWorker),
    ]
    .into_iter()
    .chain(app_items);
    let mut menus = vec![
        Menu::new("Slopty").items(app),
        Menu::new("File").items([
            MenuItem::action("New Agent", StartAgent),
            MenuItem::action("New Agent…", NewAgent),
            MenuItem::action("New Shell", NewTerminal),
            MenuItem::action("New Note", NewNote),
            MenuItem::action("Add Window…", AddWindow),
            MenuItem::separator(),
            MenuItem::action("Open File…", OpenFile),
            MenuItem::action("Open Folder…", OpenFolder),
            MenuItem::action("Open URL…", OpenUrl),
            MenuItem::separator(),
            MenuItem::action(upload, UploadFromFiles),
            MenuItem::action(download, SaveToFiles),
            MenuItem::separator(),
            MenuItem::action("Save", SaveFile),
            MenuItem::action("Save a Copy…", SaveCopy),
            MenuItem::separator(),
            MenuItem::action("Close Tile", CloseItem),
            MenuItem::action("Undo Close", UndoClose),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find…", Find),
            MenuItem::action("Find Next", FindNext),
            MenuItem::action("Find Previous", FindPrev),
        ]),
        Menu::new("View").items([
            MenuItem::action("Commands…", OpenPalette),
            MenuItem::separator(),
            MenuItem::action("Bigger Text", FontLarger),
            MenuItem::action("Smaller Text", FontSmaller),
            MenuItem::action("Actual Text Size", FontReset),
            MenuItem::separator(),
            MenuItem::action("Stream Stats", ToggleStats),
            MenuItem::action("Mute Sound", ToggleMute),
        ]),
        Menu::new("Layout").items([
            MenuItem::action("Split Right", SplitRight),
            MenuItem::action("Split Down", SplitDown),
            MenuItem::separator(),
            MenuItem::action("Pane to the Left", FocusLeft),
            MenuItem::action("Pane to the Right", FocusRight),
            MenuItem::action("Pane Above", FocusUp),
            MenuItem::action("Pane Below", FocusDown),
            MenuItem::separator(),
            MenuItem::action("Move Tile Left", MoveLeft),
            MenuItem::action("Move Tile Right", MoveRight),
            MenuItem::action("Move Tile Up", MoveUp),
            MenuItem::action("Move Tile Down", MoveDown),
            MenuItem::separator(),
            MenuItem::action("Move to Project…", MoveToProject),
            MenuItem::separator(),
            MenuItem::action("Zoom Pane", ZoomPane),
            MenuItem::action("Tab Terminal", TabTerminal),
            MenuItem::action("Equalize Panes", EqualizePanes),
            MenuItem::separator(),
            MenuItem::action("Previous Tab", PreviousTab),
            MenuItem::action("Next Tab", NextTab),
            MenuItem::action("Close Other Tabs", CloseOtherTabs),
            MenuItem::action("Previous Project", PreviousProject),
            MenuItem::action("Next Project", NextProject),
            MenuItem::action("Back", GoBack),
            MenuItem::action("Forward", GoForward),
            MenuItem::separator(),
            MenuItem::action("Next Thing Needing You", NextAttention),
        ]),
    ];
    if !ios {
        // Named "Window", AppKit lists the open windows under these and adds its own tiling.
        menus.push(Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Slopty", ShowWindow),
        ]));
    }
    // Named "Help", AppKit puts its search of the menus at the top; iPadOS keeps it last.
    menus.push(Menu::new("Help").items([
        MenuItem::action("Slopty Help", OpenHelp),
        MenuItem::action("Keyboard Shortcuts", crate::OpenKeyboardShortcuts),
    ]));
    menus
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names of `menu`'s items that run an action, in order.
    fn actions(menu: &Menu) -> Vec<String> {
        menu.items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .collect()
    }

    /// An iPad's main menu is the Mac's without the Mac's own: no Window menu and no app items
    /// passed in, Upload and Download named for the Files picker, and Keyboard Shortcuts in
    /// Help, the one place a keyboard's person looks for them there.
    #[test]
    fn an_ipads_menus_are_the_macs_without_the_macs_own() {
        let titles = |menus: &[Menu]| menus.iter().map(|m| m.name.to_string()).collect::<Vec<_>>();
        let ipad = menus(Vec::new(), true);
        let mac = menus(vec![MenuItem::separator()], false);
        assert_eq!(titles(&ipad), ["Slopty", "File", "Edit", "View", "Layout", "Help"]);
        assert_eq!(titles(&mac), ["Slopty", "File", "Edit", "View", "Layout", "Window", "Help"]);
        let file = |menus: &[Menu]| menus.iter().find(|m| m.name == "File").map(actions);
        let ipad_file = file(&ipad).unwrap_or_default();
        assert!(ipad_file.iter().any(|n| n == slopty_ui::folder::UPLOAD_FROM_FILES));
        assert!(ipad_file.iter().any(|n| n == slopty_ui::folder::SAVE_TO_FILES));
        assert!(file(&mac).unwrap_or_default().iter().any(|n| n == "Download…"));
        let help = ipad.iter().find(|m| m.name == "Help").map(actions).unwrap_or_default();
        assert_eq!(help, ["Slopty Help", "Keyboard Shortcuts"]);
    }
}
