//! The menu bar's Edit items, which name the text fields' actions (gpui-kit's): every focused
//! control answers the ones it can, and the rest are greyed.

use gpui_kit::component::input;

use super::*;

/// With a shell focused, Edit ▸ Select All and Copy take its text to the clipboard as its own
/// ⌘A and ⌘C would, and Paste is offered; Cut, Undo and Redo, which a terminal has no use for,
/// are greyed. A file's editor answers all six.
#[gpui::test]
fn the_edit_menu_reaches_whichever_control_has_the_keyboard(cx: &mut TestAppContext) {
    // Held still: a tile arriving fades in on the wall clock, and a slow machine asked the menu
    // of a frame that had not yet drawn the shell's controls.
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [(shell, tile), ..] = three_shells(&view, cx, &studio);
    view.update_in(cx, |v, _w, cx| {
        v.term_event(shell, frame(&["hello from the shell"]), cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    assert!(terminal_focused(&view, cx, shell));
    // The clipboard is the test platform's own, never the person's: start it from nothing.
    cx.update(|_w, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(String::new())));
    let offered = |cx: &mut VisualTestContext| {
        // The menu asks of the frame drawn last: draw one with the keyboard where it is now.
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
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

    let path = "/w/notes.txt";
    let file = arrives(&view, cx, &studio, ItemKind::File { path: path.into() }, 4);
    let key = studio.key;
    let text = slopty_proto::file::FileRead::Text {
        text: "a note".to_owned(),
        size: 7,
        modified_ms: WallMs::from_millis(1_000),
        final_newline: true,
        editorconfig: Vec::new(),
    };
    view.update_in(cx, |v, _w, cx| {
        v.file_read(key, path, &text, cx);
        v.focus_tile(file, cx);
    });
    cx.run_until_parked();
    // The editor draws once it has the keyboard; the menu asks of that frame.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert_eq!(offered(cx), [true; 6], "the file editor's, every one");
}
