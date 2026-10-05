//! What a page tile brings of its own: a pop-up opens a page tile beside it, a script's
//! dialog waits in the tile for its answer, ⌘F finds in the page, ⌘+ zooms it, and a
//! download gets a row. The page's side is fed in as the platform would send it.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slopty_platform::web::{Dialog, DialogKind, Download, WebEvent};

use super::*;
use crate::browser::{DownloadState, Zoom};

const HOME: &str = "http://127.0.0.1:5173/";

/// A page tile on `fake`, focused.
fn page(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, fake: &Fake) -> TileRef {
    let tile = arrives(view, cx, fake, ItemKind::Browser { url: HOME.into() }, 1);
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    tile
}

/// `event` from `tile`'s page, as the platform's web view sends it.
fn from_page(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    tile: TileRef,
    event: WebEvent,
) {
    view.update_in(cx, |v, window, cx| {
        let page = v.browser(tile.item).cloned().expect("a page view");
        page.update(cx, |page, cx| page.native_event(event, window, cx));
    });
    cx.run_until_parked();
}

fn workspace_focused(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| view.read(cx).focus.is_focused(window))
}

/// A `window.open` or a `_blank` link opens a page tile through the item path a new page
/// takes, on the page's worker; an address that is no web page opens nothing.
#[gpui::test]
fn a_pop_up_opens_a_page_tile_beside_its_page(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let tile = page(&view, cx, &fake);
    fake.drain();

    from_page(&view, cx, tile, WebEvent::Open("about:blank".into()));
    assert!(fake.drain().is_empty(), "no tile for a blank window");

    let login = "http://127.0.0.1:5173/login".to_owned();
    from_page(&view, cx, tile, WebEvent::Open(login.clone()));
    let added: Vec<ItemKind> = fake
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Items(ItemOp::Add(item)) => Some(item.kind),
            _ => None,
        })
        .collect();
    assert_eq!(added, [ItemKind::Browser { url: login }], "one new page, on the same worker");
}

/// A `confirm` hides nothing behind the page: it is a sheet in the tile, the keyboard in it,
/// and Esc answers Cancel. A `prompt`'s field starts at its default and ↩ sends what is
/// typed. Either way the keyboard goes back to the workspace.
#[gpui::test]
fn a_script_s_dialog_waits_in_its_tile_for_the_answer(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tile = page(&view, cx, &fake);
    let answers: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
    let dialog = |kind: DialogKind, message: &str| {
        let answers = Rc::clone(&answers);
        Dialog::new(kind, message.to_owned(), move |a| answers.borrow_mut().push(a))
    };

    from_page(&view, cx, tile, WebEvent::Dialog(dialog(DialogKind::Confirm, "Leave the page?")));
    assert!(cx.debug_bounds(selector("page-dialog", tile.item)).is_some(), "the sheet is up");
    let nodes = tree(cx);
    let sheet = nodes.iter().find(|n| n.role == "AlertDialog").expect("an alert dialog");
    assert_eq!(sheet.label.as_deref(), Some("Leave the page?"));
    assert!(nodes.iter().any(|n| n.is("Button", Some("Cancel"))), "{nodes:#?}");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(*answers.borrow(), [None], "Esc is Cancel");
    assert!(cx.debug_bounds(selector("page-dialog", tile.item)).is_none(), "the sheet went");
    assert!(workspace_focused(&view, cx));

    let prompt = dialog(DialogKind::Prompt { default: "Ada".into() }, "Your name?");
    from_page(&view, cx, tile, WebEvent::Dialog(prompt));
    cx.simulate_input("Grace");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(answers.borrow().last(), Some(&Some("Grace".to_owned())), "the default replaced");
    assert!(workspace_focused(&view, cx));
}

/// While an input method composes a word in a prompt's field, ↩ and Esc are the input
/// method's: the page's question stays unanswered until the word is committed.
#[gpui::test]
fn a_prompt_waits_while_its_answer_is_composed(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler as _;
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tile = page(&view, cx, &fake);
    let answers: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
    let sink = Rc::clone(&answers);
    let prompt = Dialog::new(
        DialogKind::Prompt { default: String::new() },
        "Your name?".to_owned(),
        move |a| sink.borrow_mut().push(a),
    );
    from_page(&view, cx, tile, WebEvent::Dialog(prompt));
    let field =
        view.read_with(cx, |v, cx| v.browser(tile.item).and_then(|b| b.read(cx).dialog_field()));
    let Some(field) = field else { panic!("a prompt's field") };
    cx.update(|window, cx| {
        field.update(cx, |f, cx| {
            f.replace_and_mark_text_in_range(None, "an", Some(2..2), window, cx);
        });
    });
    cx.run_until_parked();
    for key in ["enter", "escape"] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        assert!(answers.borrow().is_empty(), "{key} mid-word answers nothing");
        assert!(cx.debug_bounds(selector("page-dialog", tile.item)).is_some(), "{key}: still up");
    }
    cx.update(|window, cx| {
        field.update(cx, |f, cx| f.replace_text_in_range(None, "An", window, cx));
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(*answers.borrow(), [Some("An".to_owned())], "the word committed, ↩ sends it");
}

/// ⌘F on a page is a find bar above it, the keyboard in its field: typing asks the page,
/// its answer shows, and Esc closes the bar and gives the workspace the keyboard back.
#[gpui::test]
fn find_on_a_page_is_a_bar_above_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tile = page(&view, cx, &fake);
    let took = view.update_in(cx, |v, window, cx| v.find_in_page(window, cx));
    cx.run_until_parked();
    assert!(took, "a focused page takes ⌘F");
    assert!(cx.debug_bounds(selector("page-find", tile.item)).is_some(), "the bar is up");
    let nodes = tree(cx);
    let field = nodes.iter().find(|n| n.focused).expect("a focused node");
    assert_eq!(field.label.as_deref(), Some("Find in page"), "{field:?}");

    cx.simulate_input("needle");
    from_page(&view, cx, tile, WebEvent::Found(false));
    let (needle, found) = view.read_with(cx, |v, cx| {
        let page = v.browser(tile.item).expect("a page view").read(cx);
        (page.search_needle().map(str::to_owned), page.found())
    });
    assert_eq!((needle.as_deref(), found), (Some("needle"), Some(false)));
    let nodes = tree(cx);
    let matches = nodes.iter().find(|n| n.is("Label", Some("Matches"))).expect("the count");
    assert_eq!(matches.value.as_deref(), Some("No matches"));

    // The page counts what it finds; a count for text since typed over is not shown.
    cx.simulate_input("s");
    from_page(&view, cx, tile, WebEvent::Counted { needle: "needle".into(), count: 9 });
    from_page(&view, cx, tile, WebEvent::Found(true));
    from_page(&view, cx, tile, WebEvent::Counted { needle: "needles".into(), count: 3 });
    let nodes = tree(cx);
    let matches = nodes.iter().find(|n| n.is("Label", Some("Matches"))).expect("the count");
    assert_eq!(matches.value.as_deref(), Some("3 matches"));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("page-find", tile.item)).is_none(), "the bar closed");
    assert!(workspace_focused(&view, cx));
}

/// ⌘+ and ⌘− walk a focused page's zoom and ⌘0 puts it back, the keys reaching it rather than
/// the terminals' text size; with no page focused they are the text size's.
#[gpui::test]
fn zoom_is_the_focused_page_s(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let zoom = |view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, step| {
        view.update_in(cx, |v, _w, cx| v.zoom_page(step, cx))
    };
    assert!(!zoom(&view, cx, Zoom::In), "no page focused: not the page's");
    let tile = page(&view, cx, &fake);
    let now = |view: &Entity<WorkspaceView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| v.browser(tile.item).map(|p| p.read(cx).zoom()))
    };
    assert!(zoom(&view, cx, Zoom::In));
    assert!(zoom(&view, cx, Zoom::In));
    assert_eq!(now(&view, cx), Some(1.25));
    assert!(zoom(&view, cx, Zoom::Out));
    assert_eq!(now(&view, cx), Some(1.1));
    assert!(zoom(&view, cx, Zoom::Reset));
    assert_eq!(now(&view, cx), Some(1.0));
    // The keys themselves: ⌘+ zooms the page and leaves the terminals' text size alone.
    cx.simulate_keystrokes("cmd-=");
    cx.run_until_parked();
    assert_eq!(now(&view, cx), Some(1.1));
    assert!(view.read_with(cx, |v, _| v.font_delta).abs() < f32::EPSILON, "text size kept");
}

/// A download is a row under the page, named for its file: it says how far it has come,
/// then that it is saved, and ✕ dismisses it.
#[gpui::test]
fn a_download_is_a_row_under_its_page(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tile = page(&view, cx, &fake);
    let path = PathBuf::from("/Users/me/Downloads/report.pdf");
    from_page(&view, cx, tile, WebEvent::Download(Download::Started { id: 7, path }));
    let row = |cx: &mut VisualTestContext| {
        tree(cx).into_iter().find(|n| n.is("Group", Some("report.pdf"))).and_then(|n| n.value)
    };
    assert_eq!(row(cx).as_deref(), Some("0 B"), "under way");

    from_page(&view, cx, tile, WebEvent::Download(Download::Finished { id: 7 }));
    assert_eq!(row(cx).as_deref(), Some("Saved"));
    let state = view.read_with(cx, |v, cx| {
        v.browser(tile.item).and_then(|p| p.read(cx).downloads().first().map(|d| d.state.clone()))
    });
    assert_eq!(state, Some(DownloadState::Saved));

    let at = cx.debug_bounds("page-download-close").expect("its ✕").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(row(cx), None, "dismissed");
}

/// A page that closes its own window (`window.close()`, a pop-up done with) closes its tile
/// as ⌘W would, so "Undo close" can bring it back.
#[gpui::test]
fn a_page_that_closes_its_window_closes_its_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let tile = page(&view, cx, &fake);
    fake.drain();
    from_page(&view, cx, tile, WebEvent::Closed);
    let removed: Vec<ItemId> = fake
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Items(ItemOp::Remove(id)) => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(removed, [tile.item], "the page's own tile, and no other");
    assert!(view.read_with(cx, |v, _| v.closed.iter().any(|c| c.tile == tile)), "undoable");
}

/// The header's back and forward buttons show only while the page has history that way, so a
/// fresh page's header holds reload alone.
#[gpui::test]
fn back_and_forward_show_only_with_history_that_way(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tile = page(&view, cx, &fake);
    let shown = |cx: &mut VisualTestContext| {
        ["back", "forward", "reload"]
            .map(|part| cx.debug_bounds(selector(part, tile.item)).is_some())
    };
    assert_eq!(shown(cx), [false, false, true], "a fresh page: reload alone");
    let history = |cx: &mut VisualTestContext, back: bool, forward: bool| {
        view.update_in(cx, |v, _w, cx| {
            let page = v.browser(tile.item).cloned().expect("a page view");
            page.update(cx, |page, cx| page.set_history(back, forward, cx));
        });
        cx.run_until_parked();
    };
    history(cx, true, false);
    assert_eq!(shown(cx), [true, false, true], "a page to go back to");
    history(cx, true, true);
    assert_eq!(shown(cx), [true, true, true], "and one to go forward to, after back");
    history(cx, false, true);
    assert_eq!(shown(cx), [false, true, true], "at the first page, forward alone");
}
