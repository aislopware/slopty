//! A file tile in a headless GPUI window: real editor, real keys, the worker played by the
//! test through `set_read` and `written`.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Modifiers, TestAppContext, VisualTestContext};

use super::*;

/// What the tile asked the workspace for, in order.
type Events = Rc<RefCell<Vec<FileViewEvent>>>;

fn text_read(text: &str, newline: bool, modified_ms: u64) -> FileRead {
    let size = u64::try_from(text.len()).unwrap_or(0).saturating_add(u64::from(newline));
    FileRead::Text {
        text: text.to_owned(),
        more_lines: 0,
        size,
        modified_ms,
        final_newline: newline,
    }
}

/// A tile for `path` in its own window, its events recorded, drawn once.
fn tile<'a>(
    cx: &'a mut TestAppContext,
    path: &str,
) -> (Entity<FileView>, Events, &'a mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys(crate::workspace::key_bindings());
    });
    let path = path.to_owned();
    let (view, cx) = cx.add_window_view(|window, cx| {
        FileView::new(ItemId::new(), &path, Theme::default(), window, cx)
    });
    let events: Events = Rc::default();
    let sink = Rc::clone(&events);
    cx.update(|_window, cx| {
        cx.subscribe(&view, move |_view, event: &FileViewEvent, _cx| {
            sink.borrow_mut().push(event.clone());
        })
        .detach();
    });
    cx.run_until_parked();
    (view, events, cx)
}

/// The worker answers with `read`; the next frame puts it in the editor.
fn arrives(view: &Entity<FileView>, cx: &mut VisualTestContext, read: FileRead) {
    view.update(cx, |v, cx| v.set_read(read, cx));
    cx.run_until_parked();
}

/// The caret goes to the start of the text and `typed` goes in, as keys would.
fn types(view: &Entity<FileView>, cx: &mut VisualTestContext, typed: &str) {
    view.update_in(cx, |v, window, cx| {
        v.focus(window, cx);
        v.editor().update(cx, |e, cx| e.set_selected_range(0..0, cx));
    });
    cx.run_until_parked();
    cx.simulate_input(typed);
    cx.run_until_parked();
}

fn text(view: &Entity<FileView>, cx: &VisualTestContext) -> String {
    view.read_with(cx, FileView::text)
}

fn click(cx: &mut VisualTestContext, selector: String) {
    let selector: &'static str = Box::leak(selector.into_boxed_str());
    let bounds = cx.debug_bounds(selector);
    assert!(bounds.is_some(), "{selector} is drawn");
    if let Some(bounds) = bounds {
        cx.simulate_click(bounds.center(), Modifiers::none());
    }
    cx.run_until_parked();
}

#[gpui::test]
fn cmd_s_sends_the_edit_based_on_the_version_it_started_from(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/src/main.rs");
    arrives(&view, cx, text_read("fn a() {}\nlet b = 1;", true, 1_000));
    assert_eq!(text(&view, cx), "fn a() {}\nlet b = 1;");
    assert!(!view.read_with(cx, |v, _| v.dirty()), "a fresh read is clean");

    types(&view, cx, "// hi\n");
    assert!(view.read_with(cx, |v, _| v.dirty()), "typing makes it dirty");
    cx.simulate_keystrokes("cmd-s");
    assert_eq!(
        events.borrow().as_slice(),
        [FileViewEvent::Save {
            text: "// hi\nfn a() {}\nlet b = 1;\n".to_owned(),
            base_modified_ms: Some(1_000),
        }],
        "the whole text, the file's final newline kept, based on the read"
    );
    assert!(view.read_with(cx, |v, _| v.saving()));
    // ⌘S again while the first is out sends nothing.
    cx.simulate_keystrokes("cmd-s");
    assert_eq!(events.borrow().len(), 1);

    view.update(cx, |v, cx| {
        v.written(WriteResult::Saved { size: 26, modified_ms: 2_000 }, cx);
    });
    view.read_with(cx, |v, _| {
        assert!(!v.dirty() && !v.saving() && v.trouble().is_none(), "saved and clean");
    });
    // The watch's echo of the save changes nothing.
    arrives(&view, cx, text_read("// hi\nfn a() {}\nlet b = 1;", true, 2_000));
    view.read_with(cx, |v, _| assert!(!v.dirty() && v.trouble().is_none()));
    // A clean tile with nothing to save sends nothing.
    cx.simulate_keystrokes("cmd-s");
    assert_eq!(events.borrow().len(), 1);
}

#[gpui::test]
fn a_file_without_a_final_newline_is_saved_without_one(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/notes.txt");
    arrives(&view, cx, text_read("one", false, 5));
    types(&view, cx, "z");
    view.update(cx, FileView::save);
    assert_eq!(
        events.borrow().as_slice(),
        [FileViewEvent::Save { text: "zone".to_owned(), base_modified_ms: Some(5) }]
    );
}

#[gpui::test]
fn a_change_on_disk_under_an_edit_is_a_conflict_offered_inline(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/a.md");
    arrives(&view, cx, text_read("one\ntwo", true, 1_000));
    types(&view, cx, "mine ");
    // An agent rewrote the file meanwhile.
    arrives(&view, cx, text_read("one\nTWO", true, 1_500));
    view.read_with(cx, |v, _| {
        assert_eq!(v.trouble(), Some(&Trouble::Conflict));
        assert!(v.dirty(), "the edit is kept");
    });
    assert_eq!(text(&view, cx), "mine one\ntwo");
    // ⌘S does not write over it; the bar offers the two ways out.
    cx.simulate_keystrokes("cmd-s");
    assert!(events.borrow().is_empty());
    let id = view.read_with(cx, |v, _| *v.id().as_uuid());
    click(cx, format!("file-overwrite-{id}"));
    assert_eq!(
        events.borrow().as_slice(),
        [FileViewEvent::Save { text: "mine one\ntwo\n".to_owned(), base_modified_ms: None }],
        "overwrite writes whatever the disk has"
    );
    view.read_with(cx, |v, _| assert_eq!(v.trouble(), None));
}

#[gpui::test]
fn reload_drops_the_edit_for_the_disk_s_text(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/a.md");
    arrives(&view, cx, text_read("one\ntwo", true, 1_000));
    types(&view, cx, "mine ");
    cx.simulate_keystrokes("cmd-s");
    // The worker refused: the file changed since the read.
    view.update(cx, |v, cx| v.written(WriteResult::Conflict { modified_ms: 1_700 }, cx));
    view.read_with(cx, |v, _| assert_eq!(v.trouble(), Some(&Trouble::Conflict)));
    let id = view.read_with(cx, |v, _| *v.id().as_uuid());
    click(cx, format!("file-reload-{id}"));
    assert_eq!(events.borrow().last(), Some(&FileViewEvent::Reload), "the file is read again");
    arrives(&view, cx, text_read("theirs", true, 1_700));
    assert_eq!(text(&view, cx), "theirs");
    view.read_with(cx, |v, _| assert!(!v.dirty() && v.trouble().is_none()));
}

#[gpui::test]
fn a_clean_tile_takes_a_change_on_disk_and_tints_the_lines(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/src/lib.rs");
    arrives(&view, cx, text_read("a\nb\nc", true, 1));
    arrives(&view, cx, text_read("a\nB\nc\nd", true, 2));
    assert_eq!(text(&view, cx), "a\nB\nc\nd");
    view.read_with(cx, |v, cx| {
        assert_eq!(v.changed(), [1, 3]);
        assert_eq!(v.trouble(), None);
        assert!(!v.dirty());
        assert_eq!(v.reading_line(cx), Some(2), "the caret lands on the first change");
    });
    assert!(events.borrow().is_empty(), "a silent reload asks nothing");
}

#[gpui::test]
fn a_clipped_file_opens_read_only_with_the_reason(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/big.log");
    let read = FileRead::Text {
        text: "first".to_owned(),
        more_lines: 12,
        size: 40_000,
        modified_ms: 1,
        final_newline: false,
    };
    arrives(&view, cx, read);
    view.read_with(cx, |v, _| {
        assert_eq!(v.read_only(), Some("Read-only: longer than 2000 lines"));
    });
    types(&view, cx, "x");
    assert_eq!(text(&view, cx), "first", "typing changes nothing");
    cx.simulate_keystrokes("cmd-s");
    assert!(events.borrow().is_empty(), "nothing to save");
    let id = view.read_with(cx, |v, _| *v.id().as_uuid());
    assert!(cx.debug_bounds(Box::leak(format!("file-bar-{id}").into_boxed_str())).is_some());
    assert_eq!(
        clipped_reason(0, FILE_BYTES + 1).as_deref(),
        Some("Read-only: larger than 512.0 KB")
    );
    assert_eq!(clipped_reason(0, 10), None);
}

#[gpui::test]
fn a_failed_save_says_why_until_the_next_keystroke(cx: &mut TestAppContext) {
    let (view, _events, cx) = tile(cx, "/w/ro.txt");
    arrives(&view, cx, text_read("x", true, 1));
    types(&view, cx, "y");
    view.update(cx, FileView::save);
    view.update(cx, |v, cx| {
        v.written(WriteResult::Failed { error: "Permission denied".to_owned() }, cx);
    });
    view.read_with(cx, |v, _| {
        assert_eq!(v.trouble(), Some(&Trouble::Failed("Permission denied".to_owned())));
        assert!(v.dirty(), "the edit is still there to save");
    });
    types(&view, cx, "z");
    view.read_with(cx, |v, _| assert_eq!(v.trouble(), None));
}

#[test]
fn hits_are_the_lines_holding_the_needle_with_smart_case() {
    let lines = ["Alpha", "beta", "alpha beta", "gamma"];
    assert_eq!(find_hits(&lines, "alpha"), [0, 2], "no capital: any case");
    assert_eq!(find_hits(&lines, "Alpha"), [0], "a capital: as typed");
    assert_eq!(find_hits(&lines, "BETA"), Vec::<usize>::new());
    assert_eq!(find_hits(&lines, ""), Vec::<usize>::new(), "an empty needle finds nothing");
}

#[test]
fn changed_lines_point_at_inserts_replacements_and_deletions() {
    assert_eq!(changed_lines("a\nb\nc", "a\nB\nc"), [1]);
    assert_eq!(changed_lines("a\nc", "a\nb\nb2\nc"), [1, 2]);
    assert_eq!(changed_lines("a\nb\nc", "a\nc"), [1]);
    assert_eq!(changed_lines("a\nb", "a"), [0], "a deleted tail points at the last line");
    assert_eq!(changed_lines("a", "a"), Vec::<usize>::new());
}

#[test]
fn sizes_read_as_a_human_would() {
    assert_eq!(size_label(512), "512 B");
    assert_eq!(size_label(1536), "1.5 KB");
    assert_eq!(size_label(3 * 1024 * 1024), "3.0 MB");
}
