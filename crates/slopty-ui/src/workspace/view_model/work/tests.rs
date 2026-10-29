use gpui::AppContext as _;
use slopty_core::WallMs;
use slopty_proto::thread::wire::{TableFrame, ThreadFrame};
use slopty_proto::thread::{BackgroundTask, Cap, Cursor, ThreadState};

use crate::conversation::thread::fixtures;
use crate::workspace::WorkspaceView;
use crate::workspace::rig::{KEY, link, mac};

/// A workspace linked to one worker whose table holds `states`' threads, the first followed
/// as a chat.
fn holding<'a>(
    states: &[ThreadState],
    cx: &'a mut gpui::TestAppContext,
) -> (gpui::Entity<WorkspaceView>, &'a mut gpui::VisualTestContext) {
    let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
    let cx = cx.add_empty_window();
    workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
    let _rx = link(&workspace, cx, mac(|_| {}), Vec::new());
    let cursor = Cursor { epoch: 1, seq: 1 };
    let rows = states.iter().map(|state| state.row(WallMs::ZERO)).collect();
    let first = states.first().map(|state| state.meta.id).expect("a thread");
    workspace.update(cx, |v, cx| {
        v.threads_linked(KEY, cx);
        v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows }, cx);
        v.view_follow(first.to_string(), cx);
        for state in states {
            let frame = ThreadFrame::Snapshot { cursor, state: Box::new(state.clone()) };
            v.thread_frame(KEY, state.meta.id, frame, cx);
        }
    });
    cx.run_until_parked();
    (workspace, cx)
}

/// A followed chat's tray and dock go out beside its transcript.
#[gpui::test]
fn a_followed_chat_publishes_its_tray(cx: &mut gpui::TestAppContext) {
    let mut state = fixtures::empty();
    state.meta.caps = vec![Cap::named(Cap::STOP_TASK)];
    state.tasks =
        vec![BackgroundTask::new("b1", BackgroundTask::SHELL, "npm run dev", WallMs::ZERO)];
    let chat = state.meta.id.to_string();
    let (workspace, cx) = holding(&[state], cx);
    let views = workspace.update(cx, |v, cx| v.view_chat_views(&chat, cx)).expect("its views");
    assert_eq!(views.work.summary, "1 command");
    assert_eq!(views.work.rows.first().map(|row| row.id.as_str()), Some("task:b1"));
}
