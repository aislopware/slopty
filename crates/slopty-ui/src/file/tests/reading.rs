//! A Markdown file's preview in a real tile: it opens on the preview, swaps with the source,
//! ticks a box into the file, and gives way to the source for anything that edits.

use super::*;

const PLAN: &str = "# Plan\n\nShip it.\n\n- [ ] build\n- [x] test\n\n```sh\nls\n```";

fn previewing(view: &Entity<FileView>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |v, _| v.previewing())
}

fn uuid(view: &Entity<FileView>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |v, _| v.id().as_uuid().to_string())
}

/// The selector `name-{id}` as the tile draws it.
fn drawn(cx: &mut VisualTestContext, selector: String) -> bool {
    cx.debug_bounds(Box::leak(selector.into_boxed_str())).is_some()
}

/// A Markdown file opens on its preview, with no editor drawn, and ⌘⇧V swaps it with the
/// source and back, the keyboard going to the editor and back to the tile.
#[gpui::test]
fn a_markdown_file_opens_on_its_preview_and_swaps_with_its_source(cx: &mut TestAppContext) {
    let (view, _events, cx) = tile(cx, "/w/PLAN.md");
    arrives(&view, cx, text_read(PLAN, true, 1));
    let id = uuid(&view, cx);
    assert!(previewing(&view, cx), "the preview first");
    assert!(drawn(cx, format!("file-preview-{id}")));
    assert!(drawn(cx, format!("file-task-{id}-0")), "a task is a box");
    assert!(drawn(cx, format!("file-code-copy-{id}-3")), "a block has its copy");
    assert!(!drawn(cx, format!("file-code-run-{id}-3")), "no shell, no run");

    view.update_in(cx, |v, window, cx| v.focus(window, cx));
    cx.simulate_keystrokes("cmd-shift-v");
    assert!(!previewing(&view, cx), "the source");
    assert!(!drawn(cx, format!("file-preview-{id}")));
    let editing =
        view.read_with(cx, |v, cx| gpui::Focusable::focus_handle(v.editor().read(cx), cx));
    assert!(cx.update(|window, _| editing.is_focused(window)), "the editor has the keys");

    cx.simulate_keystrokes("cmd-shift-v");
    assert!(previewing(&view, cx), "and back");
    assert!(cx.update(|window, _| !editing.is_focused(window)), "the tile has them");
}

/// A box ticked in the preview ticks its line and saves the file at once; under an edit not
/// saved, it ticks the line and leaves the save to ⌘S.
#[gpui::test]
fn a_box_ticked_in_the_preview_saves_its_line(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/PLAN.md");
    arrives(&view, cx, text_read(PLAN, true, 1_000));
    let id = uuid(&view, cx);
    click(cx, format!("file-task-{id}-0"));
    let ticked = PLAN.replace("- [ ] build", "- [x] build");
    assert_eq!(
        events.borrow().as_slice(),
        [FileViewEvent::Save {
            text: format!("{ticked}\n"),
            base_modified_ms: Some(WallMs::from_millis(1_000)),
        }],
        "the tick is the whole edit"
    );
    view.update(cx, |v, cx| {
        v.written(WriteResult::Saved { size: 1, modified_ms: WallMs::from_millis(2_000) }, cx);
    });

    view.update_in(cx, |v, window, cx| v.show_preview(false, window, cx));
    types(&view, cx, "Draft ");
    view.update_in(cx, |v, window, cx| v.show_preview(true, window, cx));
    cx.run_until_parked();
    click(cx, format!("file-task-{id}-1"));
    assert_eq!(events.borrow().len(), 1, "no save under an edit");
    assert!(text(&view, cx).contains("- [ ] test"), "the line ticked off all the same");
    assert!(text(&view, cx).starts_with("Draft "), "the edit kept");
}

/// A file not on disk yet opens on the source, and stays on it as it is written.
#[gpui::test]
fn a_new_markdown_file_opens_on_its_source(cx: &mut TestAppContext) {
    let (new, _events, cx) = tile(cx, "/w/new.md");
    arrives(&new, cx, FileRead::Absent { editorconfig: Vec::new() });
    assert!(!previewing(&new, cx), "a new file is written");
    types(&new, cx, "# Idea");
    assert!(!previewing(&new, cx), "and stays the source as it is written");
}

/// A file opened at a line opens on the source: a line is a place in it.
#[gpui::test]
fn a_markdown_file_opened_at_a_line_opens_on_its_source(cx: &mut TestAppContext) {
    let (at, _events, cx) = tile(cx, "/w/at.md");
    at.update(cx, |v, cx| v.focus_line(Some(5), cx));
    arrives(&at, cx, text_read(PLAN, true, 1));
    assert!(!previewing(&at, cx));
}

/// ⌘F in the preview goes to the source, where the hits are marked.
#[gpui::test]
fn find_in_the_preview_goes_to_the_source(cx: &mut TestAppContext) {
    let (read, _events, cx) = tile(cx, "/w/read.markdown");
    arrives(&read, cx, text_read(PLAN, true, 1));
    assert!(previewing(&read, cx));
    read.update_in(cx, |v, window, cx| v.focus(window, cx));
    cx.simulate_keystrokes("cmd-f");
    assert!(!previewing(&read, cx), "find works on the source");
    assert!(read.read_with(cx, |v, _| v.finding()));
}

/// A file not Markdown has no preview.
#[gpui::test]
fn code_has_no_preview(cx: &mut TestAppContext) {
    let (code, _events, cx) = tile(cx, "/w/lib.rs");
    arrives(&code, cx, text_read(PLAN, true, 1));
    assert!(!code.read_with(cx, |v, _| v.has_preview() || v.previewing()));
}

#[test]
fn markdown_goes_by_its_extensions() {
    for path in ["/w/README.md", "/w/a.MARKDOWN", "notes.mdown", "/w/x.mkd"] {
        assert!(is_markdown(path), "{path}");
    }
    for path in ["/w/md", "/w/.md", "/w/a.mdx.rs", "/w/md/notes.txt"] {
        assert!(!is_markdown(path), "{path}");
    }
}
