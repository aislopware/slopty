//! Drafts survive the app: what a thread's composer held when the app went comes back in its
//! thread's tile at the next launch, and a sent message leaves nothing behind.

use super::*;

/// A thread tile of `thread` on a linked studio, its view made.
fn thread_tile(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    thread: slopty_proto::thread::ThreadId,
) -> Entity<crate::conversation::thread::ThreadView> {
    let studio = connect(view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let tile = arrives(view, cx, &studio, ItemKind::Thread { thread }, 1);
    cx.run_until_parked();
    view.read_with(cx, |v, _| v.thread_item(tile.item).cloned()).expect("the tile's thread view")
}

#[gpui::test]
fn a_composer_s_words_come_back_after_a_relaunch(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("drafts.json");
    let thread = slopty_proto::thread::ThreadId::new();
    {
        let (view, cx) = workspace(cx);
        view.update(cx, |v, cx| v.set_drafts_path(path.clone(), cx));
        let composer = thread_tile(&view, cx, thread);
        composer.update_in(cx, |c, window, cx| c.restore_draft("Half a thought", window, cx));
        view.update(cx, WorkspaceView::keep_drafts_now);
    }
    let (view, cx) = workspace(cx);
    view.update(cx, |v, cx| v.set_drafts_path(path.clone(), cx));
    let composer = thread_tile(&view, cx, thread);
    assert_eq!(
        composer.read_with(cx, crate::conversation::thread::ThreadView::draft),
        "Half a thought",
        "back in its tile"
    );

    composer.update_in(cx, |c, window, cx| c.restore_draft("", window, cx));
    view.update(cx, WorkspaceView::keep_drafts_now);
    let left = std::fs::read_to_string(&path).expect("the drafts file");
    assert!(!left.contains("Half a thought"), "an emptied composer leaves nothing: {left}");
}
