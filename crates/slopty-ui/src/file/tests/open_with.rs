//! "Open in `<editor>`" on each tile: the file at its caret's line, the folder or its selected
//! entry, the review's folder, each through the editor's link for the tile's machine; and no
//! action while nothing would open.

use std::path::PathBuf;

use gpui::{TestAppContext, VisualTestContext};
use slopty_core::ItemId;

use super::*;
use crate::file::open_with::{self, Machine, OpenInEditor};

const STUDIO: WorkerKey = WorkerKey::new(1);

/// The person's editor is Zed, and the app knows the machine `studio` with its home.
fn zed_knows_studio(cx: &mut VisualTestContext) {
    let file = "[client]\neditor = \"zed://ssh/{host}{path}:{line}\"\n";
    let link = slopty_settings::Settings::parse(file).settings.client.editor;
    cx.update(|_window, cx| {
        open_with::set_link(link, cx);
        let studio = Machine {
            name: "studio".to_owned(),
            home: Some("/Users/me".to_owned()),
            ..Machine::default()
        };
        open_with::set_machine(STUDIO, studio, cx);
    });
}

/// Whether the action is there for the file tile, the keyboard in it, as the palette asks.
fn file_offers(view: &Entity<FileView>, cx: &mut VisualTestContext) -> bool {
    view.update_in(cx, |v, window, cx| v.focus(window, cx));
    cx.run_until_parked();
    cx.update(|window, cx| window.is_action_available(&OpenInEditor, cx))
}

/// Whether the action is there for the view with `focus`, as the palette asks.
fn offered(focus: &FocusHandle, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| {
        window.focus(focus, cx);
        window.is_action_available(&OpenInEditor, cx)
    })
}

/// A file opens in the editor at the caret's line, its `~` written out under the machine's
/// home; before the app knows the machine, the tile has no such action.
#[gpui::test]
fn a_file_opens_in_the_editor_at_the_caret(cx: &mut TestAppContext) {
    let (view, _events, cx) = tile(cx, "~/src/main.rs");
    arrives(&view, cx, text_read("one\ntwo\nthree\nfour\n", true, 1));
    assert!(!file_offers(&view, cx), "no machine known, nothing to open");

    zed_knows_studio(cx);
    view.update(cx, |_v, cx| cx.notify());
    cx.run_until_parked();
    assert!(file_offers(&view, cx));
    let third = "one\ntwo\n".len();
    view.update(cx, |v, cx| v.editor().update(cx, |e, cx| e.set_selected_range(third..third, cx)));
    cx.run_until_parked();
    cx.dispatch_action(OpenInEditor);
    assert_eq!(cx.opened_url().as_deref(), Some("zed://ssh/studio/Users/me/src/main.rs:3"));
}

/// A folder opens itself while no entry is selected.
#[gpui::test]
fn a_folder_opens_in_the_editor(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        crate::folder::FolderView::new(ItemId::new(), STUDIO, "/srv/app", Theme::default(), cx)
    });
    zed_knows_studio(cx);
    view.update(cx, |_v, cx| cx.notify());
    cx.run_until_parked();
    let focus = view.read_with(cx, gpui::Focusable::focus_handle);
    assert!(offered(&focus, cx));
    cx.dispatch_action(OpenInEditor);
    assert_eq!(cx.opened_url().as_deref(), Some("zed://ssh/studio/srv/app:1"));
}

/// A review opens the folder it reviews, on the machine its hub names.
#[gpui::test]
fn a_review_opens_its_folder_in_the_editor(cx: &mut TestAppContext) {
    let hub = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.new(|_| crate::conversation::thread::ThreadHub::new("studio".to_owned(), None))
    });
    let (view, cx) = cx.add_window_view(move |window, cx| {
        crate::review::ReviewView::folder(hub, "~/w/repo".to_owned(), Theme::default(), window, cx)
    });
    zed_knows_studio(cx);
    view.update(cx, |_v, cx| cx.notify());
    cx.run_until_parked();
    let focus = view.read_with(cx, gpui::Focusable::focus_handle);
    assert!(offered(&focus, cx));
    cx.dispatch_action(OpenInEditor);
    assert_eq!(cx.opened_url().as_deref(), Some("zed://ssh/studio/Users/me/w/repo:1"));
}

/// With no editor set, a Mac opens a file of this Mac with the system's handler for its type,
/// as a `file://` link; an iPhone or iPad offers nothing.
#[gpui::test]
fn with_no_editor_this_mac_s_file_opens_with_the_system(cx: &mut TestAppContext) {
    let (view, _events, cx) = tile(cx, "/Users/me/a b.md");
    cx.update(|_window, cx| {
        let here = Machine { name: "here".to_owned(), here: true, ..Machine::default() };
        open_with::set_machine(STUDIO, here, cx);
    });
    view.update(cx, |_v, cx| cx.notify());
    cx.run_until_parked();
    let opening = cx.update(|_window, cx| open_with::opening(STUDIO, "/Users/me/a b.md", None, cx));
    if cfg!(target_os = "macos") {
        assert!(file_offers(&view, cx));
        assert_eq!(opening, Some(open_with::Opening::File(PathBuf::from("/Users/me/a b.md"))));
        cx.dispatch_action(OpenInEditor);
        assert_eq!(cx.opened_url().as_deref(), Some("file:///Users/me/a%20b.md"));
    } else {
        assert!(!file_offers(&view, cx));
        assert_eq!(opening, None);
    }
}
