//! The menu bar's Edit items, which name the text fields' actions (gpui-kit's): every focused
//! control answers the ones it can, and the rest are greyed.

use gpui_kit::component::input;

use super::*;

/// With a shell focused, Edit ▸ Select All and Copy take its text to the clipboard as its own
/// ⌘A and ⌘C would, and Paste is offered; Cut, Undo and Redo, which a terminal has no use for,
/// are greyed. A note's editor answers all six.
#[gpui::test]
fn the_edit_menu_reaches_whichever_control_has_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [(shell, tile), ..] = three_shells(&view, cx, &studio);
    view.update_in(cx, |v, _w, cx| {
        v.term_event(shell, frame(&["hello from the shell"]), cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    assert!(terminal_focused(&view, cx, shell));
    let offered = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            [
                window.is_action_available(&input::Undo, cx),
                window.is_action_available(&input::Redo, cx),
                window.is_action_available(&input::Cut, cx),
                window.is_action_available(&input::Copy, cx),
                window.is_action_available(&input::Paste, cx),
                window.is_action_available(&input::SelectAll, cx),
            ]
        })
    };
    assert_eq!(offered(cx), [false, false, false, true, true, true], "the shell's");
    cx.dispatch_action(input::SelectAll);
    cx.dispatch_action(input::Copy);
    cx.run_until_parked();
    let copied = cx.update(|_w, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert!(copied.is_some_and(|t| t.contains("hello from the shell")), "copied");

    let note = arrives(&view, cx, &studio, ItemKind::Note { text: "a note".into() }, 4);
    view.update_in(cx, |v, _w, cx| v.focus_tile(note, cx));
    cx.run_until_parked();
    // Writing in it, as a click into its text would start.
    view.update_in(cx, |v, window, cx| {
        let editor = v.notes.get(&note.item).cloned().expect("the note's view");
        editor.update(cx, |n, cx| n.focus(window, cx));
    });
    cx.run_until_parked();
    // The note draws its editor once it has the keyboard; the menu asks of that frame.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert_eq!(offered(cx), [true; 6], "the note editor's, every one");
}
